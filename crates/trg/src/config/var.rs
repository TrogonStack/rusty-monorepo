use std::collections::HashMap;
use std::fmt;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use crate::secrets::onepassword::{OnePasswordReference, OpReferenceError};
use crate::secrets::{
    BackendKind, GcpReference, GcpSecretId, GcpSecretIdError, KeyError, KeychainReference, OpenbaoReference, PathError,
    SecretAddress, SecretKey, SecretPath, SecretsSection,
};

/// How a declaration addresses its value, in the one vocabulary it chose.
///
/// Each backend kind speaks exactly one of these, and each has a spelling no
/// other shares, so the shape of a table alone says which was meant and a
/// table that blends two cannot be built. Which vocabulary the named backend
/// speaks is only known once the whole document is read, so that comparison is
/// [`SecretVar::resolve_at`]'s; everything that can be refused without it is
/// refused here, while the file is parsed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RawAddress {
    /// `path` and `key`, the vocabulary of `keychain` and `openbao`.
    PathKey { path: String, key: String },
    /// `ref`, the vocabulary of `onepassword`.
    Reference(String),
    /// `secret`, the vocabulary of `gcp_secret_manager`, whose secrets hold
    /// one value each and so have an id and no fields.
    Secret(String),
}

impl RawAddress {
    pub fn vocabulary(&self) -> Vocabulary {
        match self {
            Self::PathKey { .. } => Vocabulary::PathKey,
            Self::Reference(_) => Vocabulary::Reference,
            Self::Secret(_) => Vocabulary::Secret,
        }
    }

    /// The fields as the file spells them, in the order they are written.
    pub fn fields(&self) -> Vec<(&'static str, &str)> {
        match self {
            Self::PathKey { path, key } => vec![("path", path), ("key", key)],
            Self::Reference(reference) => vec![("ref", reference)],
            Self::Secret(secret) => vec![("secret", secret)],
        }
    }

    /// The address fields as the optional spellings a table or a flag set
    /// offers, which is the one place a partial or blended shape can show up.
    pub fn from_parts(
        backend: &str,
        path: Option<String>,
        key: Option<String>,
        reference: Option<String>,
        secret: Option<String>,
    ) -> Result<Self, AddressShapeError> {
        let backend = backend.to_string();
        match (path, key, reference, secret) {
            (Some(path), Some(key), None, None) => Ok(Self::PathKey { path, key }),
            (None, None, Some(reference), None) => Ok(Self::Reference(reference)),
            (None, None, None, Some(secret)) => Ok(Self::Secret(secret)),
            (Some(_), None, None, None) => Err(AddressShapeError::Incomplete {
                backend,
                present: "path",
                missing: "key",
            }),
            (None, Some(_), None, None) => Err(AddressShapeError::Incomplete {
                backend,
                present: "key",
                missing: "path",
            }),
            (None, None, None, None) => Err(AddressShapeError::Empty { backend }),
            (path, key, reference, secret) => {
                let given = [
                    ("path", path.is_some()),
                    ("key", key.is_some()),
                    ("ref", reference.is_some()),
                    ("secret", secret.is_some()),
                ]
                .into_iter()
                .filter(|(_, present)| *present)
                .map(|(field, _)| format!("`{field}`"))
                .collect::<Vec<_>>()
                .join(", ");
                Err(AddressShapeError::Mixed { backend, given })
            }
        }
    }
}

/// The three ways a backend can be addressed, named apart from any value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vocabulary {
    PathKey,
    Reference,
    Secret,
}

impl Vocabulary {
    /// What a backend of this kind is addressed with.
    pub fn of(kind: BackendKind) -> Self {
        match kind {
            BackendKind::Keychain | BackendKind::Openbao => Self::PathKey,
            BackendKind::OnePassword => Self::Reference,
            BackendKind::GcpSecretManager => Self::Secret,
        }
    }

    fn spelled(self, site: &VarSite) -> &'static str {
        match (self, site) {
            (Self::PathKey, VarSite::Table { .. }) => "`path` and `key`",
            (Self::PathKey, VarSite::Flags) => "`--path` and `--key`",
            (Self::Reference, VarSite::Table { .. }) => "`ref`",
            (Self::Reference, VarSite::Flags) => "`--ref`",
            (Self::Secret, VarSite::Table { .. }) => "`secret`",
            (Self::Secret, VarSite::Flags) => "`--secret`",
        }
    }

    fn fix(self, site: &VarSite, backend: &str) -> String {
        match (self, site) {
            (Self::PathKey, VarSite::Table { .. }) => {
                format!(r#"replace with: {{ backend = "{backend}", path = "...", key = "..." }}"#)
            }
            (Self::PathKey, VarSite::Flags) => "use instead: --path <path> --key <key>".to_string(),
            (Self::Reference, VarSite::Table { .. }) => {
                format!(r#"replace with: {{ backend = "{backend}", ref = "op://<vault>/<item>/<field>" }}"#)
            }
            (Self::Reference, VarSite::Flags) => r#"use instead: --ref "op://<vault>/<item>/<field>""#.to_string(),
            (Self::Secret, VarSite::Table { .. }) => {
                format!(r#"replace with: {{ backend = "{backend}", secret = "<secret-id>" }}"#)
            }
            (Self::Secret, VarSite::Flags) => "use instead: --secret <secret-id>".to_string(),
        }
    }
}

/// Why a table is not shaped like any of the three address vocabularies.
///
/// Raised while the file is parsed, because none of it depends on which
/// backend the table names.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AddressShapeError {
    #[error("backend `{backend}`: `{present}` needs `{missing}` as well; `{missing}` is missing")]
    Incomplete {
        backend: String,
        present: &'static str,
        missing: &'static str,
    },

    #[error(
        "backend `{backend}`: no address; give `path` and `key` (keychain, openbao), `ref` (onepassword) or `secret` (gcp_secret_manager)"
    )]
    Empty { backend: String },

    #[error(
        "backend `{backend}`: {given} mix address spellings; use exactly one of `path` and `key`, `ref`, or `secret`"
    )]
    Mixed { backend: String, given: String },
}

/// A secret declaration exactly as the file spelled it, before anything knows
/// which backend it names.
///
/// The address is already one of the three vocabularies, because that much
/// does not depend on the backend: a partial or blended table is refused by
/// the parser. What stays open is whether the named backend speaks that
/// vocabulary, and that is not known until the whole document has been read,
/// since the `[secrets.backends]` table may well be written below the server
/// that uses it. Resolution into a [`SecretVar`] is the second phase.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Hash)]
#[serde(try_from = "RawSecretVarFields")]
pub struct RawSecretVar {
    /// The `[secrets.backends.<name>]` entry to read from, named here rather
    /// than inherited from the server, so a var says where it comes from
    /// without the reader tracing it through anything.
    pub backend: String,
    pub address: RawAddress,
}

/// The fields of a declaration table as the file may write them, before they
/// are checked to form one vocabulary.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawSecretVarFields {
    pub backend: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    /// Spelled `ref` in the file, which is a Rust keyword.
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
}

impl TryFrom<RawSecretVarFields> for RawSecretVar {
    type Error = AddressShapeError;

    fn try_from(fields: RawSecretVarFields) -> Result<Self, Self::Error> {
        let address = RawAddress::from_parts(
            &fields.backend,
            fields.path,
            fields.key,
            fields.reference,
            fields.secret,
        )?;
        Ok(Self {
            backend: fields.backend,
            address,
        })
    }
}

impl RawSecretVar {
    pub fn new(backend: String, address: RawAddress) -> Self {
        Self { backend, address }
    }

    /// A declaration addressed by path and key.
    ///
    /// For callers that only know that shape.
    pub fn path_key(backend: String, path: &SecretPath, key: &SecretKey) -> Self {
        Self::new(
            backend,
            RawAddress::PathKey {
                path: path.as_str().to_string(),
                key: key.as_str().to_string(),
            },
        )
    }

    /// The inline table that declares this var in a `vars` table, so the
    /// address just written and the address that will be read cannot be
    /// spelled differently.
    ///
    /// Escaped because a path may hold a character that would otherwise end
    /// the TOML string early.
    pub fn declaration(&self) -> String {
        let mut out = format!("{{ backend = {}", toml_basic_string(&self.backend));
        for (field, value) in self.address.fields() {
            out.push_str(&format!(", {field} = {}", toml_basic_string(value)));
        }
        out.push_str(" }");
        out
    }
}

impl fmt::Display for RawSecretVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "backend `{}`", self.backend)?;
        match &self.address {
            RawAddress::Reference(value) | RawAddress::Secret(value) => write!(f, " at `{value}`"),
            RawAddress::PathKey { path, key } => write!(f, " at `{path}`, key `{key}`"),
        }
    }
}

/// Deserialise `T` out of a TOML value already read, keeping its own message.
///
/// Used where a shape is chosen by what the table holds rather than by trying
/// each variant in turn, because trying each in turn reports only that none
/// of them matched and loses the one message that names the mistake.
pub(crate) fn from_toml_value<'de, T: Deserialize<'de>, E: serde::de::Error>(value: toml::Value) -> Result<T, E> {
    T::deserialize(value).map_err(|e| E::custom(e.message()))
}

/// Where an address was written, so an error can point at it and offer the
/// fix in the spelling that place uses.
///
/// Carried as a value rather than formatted into the message at the throw
/// site, because the same resolution runs over two config tables and over the
/// `trg secret` flags, and only the caller knows which of them it is walking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VarSite {
    Table {
        table: String,
        name: String,
    },
    /// The `trg secret` address flags, where the remedy is another invocation
    /// rather than an edit, and so is spelled with flags rather than as TOML.
    Flags,
}

impl VarSite {
    pub fn mcp_var(server: &str, name: &str) -> Self {
        Self::Table {
            table: format!("[mcp.servers.{server}.vars]"),
            name: name.to_string(),
        }
    }

    pub fn exec_env(entry: &str, name: &str) -> Self {
        Self::Table {
            table: format!("[exec.{entry}.env]"),
            name: name.to_string(),
        }
    }

    /// The one address list of a `[secrets.vars.<name>]` table.
    pub fn named_secret(name: &str) -> Self {
        Self::Table {
            table: format!("[secrets.vars.{name}]"),
            name: "secret".to_string(),
        }
    }

    /// One side of a `[secrets.mirrors.<name>]` table.
    pub fn mirror(name: &str, side: &str) -> Self {
        Self::Table {
            table: format!("[secrets.mirrors.{name}]"),
            name: side.to_string(),
        }
    }

    /// One `--fallback` of `trg secret get`, which is written as the same
    /// inline table a config would hold, so its errors are spelled as TOML too.
    pub fn fallback_flag(position: usize) -> Self {
        Self::Table {
            table: "--fallback".to_string(),
            name: format!("#{position}"),
        }
    }
}

impl fmt::Display for VarSite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Table { table, name } => write!(f, "{table} {name}"),
            Self::Flags => f.write_str("this invocation"),
        }
    }
}

/// Why a [`RawSecretVar`] is not a valid address for the backend it names.
#[derive(Debug, thiserror::Error)]
pub enum SecretVarError {
    #[error("{site} names backend `{backend}`, which is not declared; declared: {declared}")]
    UnknownBackend {
        site: VarSite,
        backend: String,
        declared: String,
    },

    #[error("{site} names backend `{backend}`, but no secrets backends are declared; add a `[secrets.backends.{backend}]` section")]
    NoBackendsDeclared { site: VarSite, backend: String },

    /// The replacement is spelled out because there is deliberately no
    /// compatibility bridge between vocabularies, so the error has to be the
    /// instructions.
    #[error(
        "{site} names backend `{backend}` of kind `{kind}`, which is addressed with {}, not {}\n  {}",
        Vocabulary::of(*kind).spelled(site),
        given.spelled(site),
        Vocabulary::of(*kind).fix(site, backend)
    )]
    WrongVocabulary {
        site: VarSite,
        backend: String,
        kind: BackendKind,
        given: Vocabulary,
    },

    #[error("{site}: invalid `secret`: {cause}")]
    SecretId {
        site: VarSite,
        #[source]
        cause: GcpSecretIdError,
    },

    #[error("{site}: `fallbacks` must list at least one address")]
    EmptyFallbacks { site: VarSite },

    #[error("{site}: {cause}")]
    Reference {
        site: VarSite,
        #[source]
        cause: OpReferenceError,
    },

    #[error("{site}: invalid `path`: {cause}")]
    Path {
        site: VarSite,
        #[source]
        cause: PathError,
    },

    #[error("{site}: invalid `key`: {cause}")]
    Key {
        site: VarSite,
        #[source]
        cause: KeyError,
    },
}

/// One value in a secrets backend, addressed the way that backend addresses
/// things.
///
/// A declaration is the secret's whole identity and names nothing about who
/// reads it, so the same inline table can be pasted into as many servers as
/// need that value.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SecretVar {
    backend: String,
    address: SecretAddress,
}

impl SecretVar {
    pub fn new(backend: String, address: SecretAddress) -> Self {
        Self { backend, address }
    }

    /// Resolve a raw declaration against the backends the document declares.
    ///
    /// The kind of the named backend is what decides which addressing fields
    /// are required, which is why this cannot happen while deserialising a
    /// single table.
    pub fn resolve(raw: &RawSecretVar, secrets: &SecretsSection, site: VarSite) -> Result<Self, SecretVarError> {
        let Some(kind) = secrets.kind_of(&raw.backend) else {
            let declared = secrets.declared_names();
            return Err(if declared.is_empty() {
                SecretVarError::NoBackendsDeclared {
                    site,
                    backend: raw.backend.clone(),
                }
            } else {
                SecretVarError::UnknownBackend {
                    site,
                    backend: raw.backend.clone(),
                    declared: declared.join(", "),
                }
            });
        };

        Self::resolve_at(kind, raw, site)
    }

    /// The same, against a kind the caller has already looked up.
    ///
    /// `trg secret` names a backend the registry resolved rather than a
    /// `[secrets.backends]` table it can hand over, and the rule about which
    /// vocabulary that backend speaks has to be the same one either way, or
    /// a var that loads would be refused on the command line.
    pub fn resolve_at(kind: BackendKind, raw: &RawSecretVar, site: VarSite) -> Result<Self, SecretVarError> {
        let address = parse_address(kind, raw, &site)?;
        Ok(Self {
            backend: raw.backend.clone(),
            address,
        })
    }

    pub fn backend(&self) -> &str {
        &self.backend
    }

    pub fn address(&self) -> &SecretAddress {
        &self.address
    }

    /// The `trg secret put` invocation that would write this var, so an error
    /// about a missing one can hand back the fix rather than describe it.
    ///
    /// `None` for a backend `trg` cannot write to: offering a command that is
    /// guaranteed to fail is worse than offering nothing, because it sends
    /// someone to fix the wrong thing.
    ///
    /// Quoted, because a path is allowed a space and a command offered as the
    /// fix has to survive being pasted.
    pub fn put_command(&self) -> Option<String> {
        if !self.address.backend_kind().is_writable() {
            return None;
        }
        if let SecretAddress::GcpSecretManager(r) = &self.address {
            return Some(format!(
                "trg secret put --backend {} --secret {}",
                crate::shell::quote_for_shell(&self.backend),
                crate::shell::quote_for_shell(r.secret().as_str())
            ));
        }
        let (path, key) = self.address.path_key()?;
        let q = crate::shell::quote_for_shell;
        Some(format!(
            "trg secret put --backend {} --path {} --key {}",
            q(&self.backend),
            q(path.as_str()),
            q(key.as_str())
        ))
    }

    /// The inline table that declares this var in a server's `vars`.
    ///
    /// Rendered through [`RawSecretVar`], the shape the file actually holds,
    /// so what is offered here is by construction what the loader will accept
    /// back.
    pub fn declaration(&self) -> String {
        self.as_raw().declaration()
    }

    fn as_raw(&self) -> RawSecretVar {
        let address = match &self.address {
            SecretAddress::Keychain(r) => RawAddress::PathKey {
                path: r.path().as_str().to_string(),
                key: r.key().as_str().to_string(),
            },
            SecretAddress::Openbao(r) => RawAddress::PathKey {
                path: r.path().as_str().to_string(),
                key: r.key().as_str().to_string(),
            },
            SecretAddress::GcpSecretManager(r) => RawAddress::Secret(r.secret().as_str().to_string()),
            SecretAddress::OnePassword(r) => RawAddress::Reference(r.to_string()),
        };
        RawSecretVar::new(self.backend.clone(), address)
    }
}

/// One value, and the addresses it may be read from, in order.
///
/// A plain `{ backend, ... }` declaration is a read with nothing after the
/// first address. `fallbacks = [...]` on that declaration is the opt-in for
/// more: each entry is a full [`SecretVar`], because a 1Password reference and
/// an OpenBao path and key are shaped differently and there is no address both
/// could share.
///
/// This is the only type a fallback exists on, and nothing `trg` writes is
/// one. Credentials are written through a backend named by a bare string, and
/// `trg secret put` takes a single address, so a write that quietly lands in
/// the second of two stores is not a state these types can express.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SecretRead {
    primary: SecretVar,
    fallbacks: Vec<SecretVar>,
}

impl SecretRead {
    pub fn single(primary: SecretVar) -> Self {
        Self {
            primary,
            fallbacks: Vec::new(),
        }
    }

    /// `None` for an empty list, which is a read of nothing.
    pub fn with_fallbacks(primary: SecretVar, fallbacks: Vec<SecretVar>) -> Self {
        Self { primary, fallbacks }
    }

    pub fn from_addresses(vars: Vec<SecretVar>) -> Option<Self> {
        let mut vars = vars.into_iter();
        let primary = vars.next()?;
        Some(Self {
            primary,
            fallbacks: vars.collect(),
        })
    }

    pub fn primary(&self) -> &SecretVar {
        &self.primary
    }

    pub fn fallbacks(&self) -> &[SecretVar] {
        &self.fallbacks
    }

    pub fn has_fallbacks(&self) -> bool {
        !self.fallbacks.is_empty()
    }
}

impl From<SecretVar> for SecretRead {
    fn from(primary: SecretVar) -> Self {
        Self::single(primary)
    }
}

impl fmt::Display for SecretRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.primary)?;
        for (i, fallback) in self.fallbacks.iter().enumerate() {
            let lead = if i == 0 { ", falling back to" } else { ", then" };
            write!(f, "{lead} {fallback}")?;
        }
        Ok(())
    }
}

/// Read a raw declaration in the vocabulary of one backend kind.
///
/// Split out of [`SecretVar::resolve`] so the dispatch on kind reads as the
/// one place that says how each backend is addressed. A kind added to
/// [`BackendKind`] cannot be forgotten here. The shape of the address is
/// already known to be whole, so all that is left to refuse is a vocabulary
/// the kind does not speak and a value that does not parse.
fn parse_address(kind: BackendKind, raw: &RawSecretVar, site: &VarSite) -> Result<SecretAddress, SecretVarError> {
    let given = raw.address.vocabulary();
    if given != Vocabulary::of(kind) {
        return Err(SecretVarError::WrongVocabulary {
            site: site.clone(),
            backend: raw.backend.clone(),
            kind,
            given,
        });
    }
    match (&raw.address, kind) {
        (RawAddress::PathKey { path, key }, BackendKind::Keychain | BackendKind::Openbao) => {
            let path = SecretPath::parse(path).map_err(|cause| SecretVarError::Path {
                site: site.clone(),
                cause,
            })?;
            let key = SecretKey::parse(key).map_err(|cause| SecretVarError::Key {
                site: site.clone(),
                cause,
            })?;
            Ok(match kind {
                BackendKind::Keychain => SecretAddress::Keychain(KeychainReference::new(path, key)),
                _ => SecretAddress::Openbao(OpenbaoReference::new(path, key)),
            })
        }
        (RawAddress::Secret(id), BackendKind::GcpSecretManager) => GcpSecretId::parse(id)
            .map(|id| SecretAddress::GcpSecretManager(GcpReference::new(id)))
            .map_err(|cause| SecretVarError::SecretId {
                site: site.clone(),
                cause,
            }),
        (RawAddress::Reference(reference), BackendKind::OnePassword) => OnePasswordReference::parse(reference)
            .map(|r| SecretAddress::OnePassword(Box::new(r)))
            .map_err(|cause| SecretVarError::Reference {
                site: site.clone(),
                cause,
            }),
        _ => Err(SecretVarError::WrongVocabulary {
            site: site.clone(),
            backend: raw.backend.clone(),
            kind,
            given,
        }),
    }
}

/// A TOML basic string, always on one line, because an inline table is.
fn toml_basic_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for c in raw.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl fmt::Display for SecretVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} from backend `{}`", self.address, self.backend)
    }
}

/// Secret var values already read out of their backends.
///
/// Resolution is split in two because a backend read is a network call that
/// wants a token and can be refused, while [`VarSource::Env`] is a memory
/// lookup. Fetching first, in one batch, keeps the number of round trips to
/// one per distinct path no matter how many vars reference it.
#[derive(Debug, Default)]
pub struct FetchedSecrets {
    values: HashMap<SecretRead, SecretString>,
    answered_by: HashMap<SecretRead, String>,
}

impl FetchedSecrets {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, read: impl Into<SecretRead>, value: SecretString) {
        self.values.insert(read.into(), value);
    }

    pub fn get(&self, read: &SecretRead) -> Option<&SecretString> {
        self.values.get(read)
    }

    /// Noted only for a read that listed fallbacks, because for any other the
    /// answer can only have come from its one address.
    pub fn record_answered_by(&mut self, read: SecretRead, backend: String) {
        self.answered_by.insert(read, backend);
    }

    pub fn answered_by(&self, read: &SecretRead) -> Option<&str> {
        self.answered_by.get(read).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// A value source for `[mcp.servers.<name>.vars]` entries.
///
/// `Literal` is a bare TOML string; `Env` is an inline `{ env, default? }`
/// table; `Secret` is an inline secret declaration, whose addressing fields
/// depend on the backend it names; `SecretWithFallbacks` is that same
/// declaration plus further ones to read, in order, when it cannot answer.
/// `VarSource` is intentionally accepted only inside a `vars` table, never directly
/// in `url` or header values.
///
/// Generic over the secret payload rather than holding a half-filled
/// [`SecretVar`], so that "read from the file, backend not yet known" and
/// "checked against the backend it names" are different types and a resolved
/// tree cannot be built by forgetting a step.
#[derive(Clone, Debug)]
pub enum VarSource<S = SecretVar> {
    Literal(String),
    Env { env: String, default: Option<String> },
    Secret(S),
    SecretWithFallbacks(Box<WithFallbacks<S>>),
}

/// Read by what the value is rather than by trying each variant in turn, so a
/// secret table that is shaped wrongly is reported as that table's own mistake
/// and not as "none of these matched".
impl<'de, S: Deserialize<'de>> Deserialize<'de> for VarSource<S> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_toml_value(toml::Value::deserialize(deserializer)?)
    }
}

impl<'de, S: Deserialize<'de>> VarSource<S> {
    pub(crate) fn from_toml_value<E: serde::de::Error>(value: toml::Value) -> Result<Self, E> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct EnvFields {
            env: String,
            #[serde(default)]
            default: Option<String>,
        }

        match value {
            toml::Value::String(literal) => Ok(Self::Literal(literal)),
            toml::Value::Table(table) if table.contains_key("env") => {
                let EnvFields { env, default } = from_toml_value(toml::Value::Table(table))?;
                Ok(Self::Env { env, default })
            }
            toml::Value::Table(table) if table.contains_key("fallbacks") => Ok(Self::SecretWithFallbacks(Box::new(
                from_toml_value(toml::Value::Table(table))?,
            ))),
            toml::Value::Table(table) if table.contains_key("backend") => {
                Ok(Self::Secret(from_toml_value(toml::Value::Table(table))?))
            }
            toml::Value::Table(_) => Err(E::custom(format!("invalid table, {VAR_SOURCE_EXPECTED}"))),
            other => Err(E::custom(format!(
                "invalid type: {}, {VAR_SOURCE_EXPECTED}",
                other.type_str()
            ))),
        }
    }
}

const VAR_SOURCE_EXPECTED: &str = "expected a string, or an inline table `{ env = \"NAME\", default = \"...\" }`, \
     `{ backend = \"NAME\", path = \"...\", key = \"...\" }`, \
     `{ backend = \"NAME\", ref = \"op://<vault>/<item>/<field>\" }`, \
     `{ backend = \"NAME\", secret = \"<secret-id>\" }` \
     or a secret table with `fallbacks = [ <secret tables>, ... ]`, \
     whose entries cannot carry `fallbacks` of their own";

/// A secret declaration and the ones to try after it.
///
/// The primary is flattened in rather than nested, so it is written exactly as
/// a declaration without fallbacks is, and an entry of `fallbacks` is that
/// same plain declaration and so cannot carry a list of its own.
#[derive(Clone, Debug, Deserialize)]
#[serde(bound(deserialize = "S: Deserialize<'de>"))]
pub struct WithFallbacks<S> {
    #[serde(flatten)]
    pub primary: S,
    pub fallbacks: Vec<S>,
}

/// A [`VarSource`] as deserialised, before its backend is known.
pub type RawVarSource = VarSource<RawSecretVar>;

impl<S> VarSource<S> {
    /// The backend read this source needs before it can resolve, if any.
    pub fn secret(&self) -> Option<&S> {
        match self {
            VarSource::Secret(v) => Some(v),
            VarSource::Literal(_) | VarSource::Env { .. } | VarSource::SecretWithFallbacks(_) => None,
        }
    }

    /// Whether this source reads from a secrets backend at all, in either
    /// form.
    pub fn reads_secret(&self) -> bool {
        matches!(self, VarSource::Secret(_) | VarSource::SecretWithFallbacks(_))
    }
}

impl RawVarSource {
    /// Check this source's secret, if it has one, against the declared
    /// backends.
    pub fn into_resolved(
        self,
        secrets: &SecretsSection,
        site: impl Fn() -> VarSite,
    ) -> Result<VarSource, SecretVarError> {
        Ok(match self {
            VarSource::Literal(s) => VarSource::Literal(s),
            VarSource::Env { env, default } => VarSource::Env { env, default },
            VarSource::Secret(raw) => VarSource::Secret(SecretVar::resolve(&raw, secrets, site())?),
            VarSource::SecretWithFallbacks(raw) => {
                if raw.fallbacks.is_empty() {
                    return Err(SecretVarError::EmptyFallbacks { site: site() });
                }
                VarSource::SecretWithFallbacks(Box::new(WithFallbacks {
                    primary: SecretVar::resolve(&raw.primary, secrets, site())?,
                    fallbacks: raw
                        .fallbacks
                        .iter()
                        .map(|fallback| SecretVar::resolve(fallback, secrets, site()))
                        .collect::<Result<_, _>>()?,
                }))
            }
        })
    }

    /// Resolve a backend declaration's own var, which may not itself come
    /// from a backend.
    ///
    /// A separate method from [`VarSource::resolve`] because these are read
    /// while the registry is still being built, so there is nothing to read a
    /// secret out of yet. [`crate::secrets::BackendError::SecretVar`] rejects
    /// the attempt with the config still in view; this arm is what remains if
    /// anything ever reaches here without that check.
    pub fn resolve_bootstrap(&self, _fetched: &FetchedSecrets) -> Result<String, VarResolveError> {
        match self {
            VarSource::Literal(s) => Ok(s.clone()),
            VarSource::Env { env, default } => resolve_env(env, default.as_deref()),
            VarSource::Secret(raw) => Err(VarResolveError::SecretInDeclaration(raw.backend.clone())),
            VarSource::SecretWithFallbacks(raw) => {
                Err(VarResolveError::SecretInDeclaration(raw.primary.backend.clone()))
            }
        }
    }
}

impl VarSource {
    /// The read this source needs fetched, if it has one.
    ///
    /// `None` for a source that reads no secret.
    pub fn secret_read(&self) -> Option<SecretRead> {
        match self {
            VarSource::Secret(v) => Some(SecretRead::single(v.clone())),
            VarSource::SecretWithFallbacks(v) => {
                Some(SecretRead::with_fallbacks(v.primary.clone(), v.fallbacks.clone()))
            }
            VarSource::Literal(_) | VarSource::Env { .. } => None,
        }
    }

    pub fn resolve(&self, fetched: &FetchedSecrets) -> Result<String, VarResolveError> {
        self.resolve_with_env(fetched, &|name| std::env::var(name).ok())
    }

    pub(super) fn resolve_with_env(
        &self,
        fetched: &FetchedSecrets,
        environment: &impl Fn(&str) -> Option<String>,
    ) -> Result<String, VarResolveError> {
        match self {
            VarSource::Literal(s) => Ok(s.clone()),
            VarSource::Env { env, default } => resolve_env_with(env, default.as_deref(), environment),
            // Absent here means the caller resolved without fetching first,
            // which is a wiring mistake rather than anything the config said.
            VarSource::Secret(_) | VarSource::SecretWithFallbacks(_) => {
                let read = self.secret_read().expect("a secret source has a read");
                fetched
                    .get(&read)
                    .map(|s| s.expose_secret().to_string())
                    .ok_or(VarResolveError::SecretNotFetched(read))
            }
        }
    }
}

fn resolve_env(name: &str, default: Option<&str>) -> Result<String, VarResolveError> {
    resolve_env_with(name, default, &|name| std::env::var(name).ok())
}

fn resolve_env_with(
    name: &str,
    default: Option<&str>,
    environment: &impl Fn(&str) -> Option<String>,
) -> Result<String, VarResolveError> {
    match environment(name) {
        Some(v) => Ok(v),
        None => default
            .map(str::to_string)
            .ok_or_else(|| VarResolveError::MissingEnv(name.to_string())),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VarResolveError {
    #[error("environment variable `{0}` is required but unset")]
    MissingEnv(String),

    #[error("undefined variable `{0}` referenced; declare it in `[mcp.servers.<name>.vars]`")]
    UndefinedVar(String),

    #[error("secret {0} was resolved before it was read")]
    SecretNotFetched(SecretRead),

    #[error("`[secrets.backends.{0}]` cannot take its own address from a secrets backend")]
    SecretInDeclaration(String),
}

/// A value for `[exec.<name>.env]`: either one `VarSource`, or an array of
/// them concatenated in order.
///
/// Unlike `VarTemplate` (used for `url`/headers), each array element may be a
/// full `VarSource`, including `{ env = ... }` and a secret declaration,
/// because an exec entry's `env` table has no separate `vars` table to route
/// an indirect reference through; it is already the one place these bindings
/// live.
#[derive(Clone, Debug)]
pub enum EnvValue<S = SecretVar> {
    Scalar(VarSource<S>),
    Composed(Vec<VarSource<S>>),
}

impl<'de, S: Deserialize<'de>> Deserialize<'de> for EnvValue<S> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match toml::Value::deserialize(deserializer)? {
            toml::Value::Array(items) => items
                .into_iter()
                .map(VarSource::from_toml_value)
                .collect::<Result<Vec<_>, _>>()
                .map(Self::Composed),
            single => VarSource::from_toml_value(single).map(Self::Scalar),
        }
    }
}

/// An [`EnvValue`] as deserialised, before its backends are known.
pub type RawEnvValue = EnvValue<RawSecretVar>;

impl<S> EnvValue<S> {
    /// Every backend read this value needs before it can resolve.
    pub fn secrets(&self) -> Vec<&S> {
        match self {
            EnvValue::Scalar(v) => v.secret().into_iter().collect(),
            EnvValue::Composed(segs) => segs.iter().filter_map(|s| s.secret()).collect(),
        }
    }
}

impl EnvValue {
    /// Every read this value needs fetched before it can resolve.
    pub fn secret_reads(&self) -> Vec<SecretRead> {
        match self {
            EnvValue::Scalar(v) => v.secret_read().into_iter().collect(),
            EnvValue::Composed(segs) => segs.iter().filter_map(VarSource::secret_read).collect(),
        }
    }
}

impl RawEnvValue {
    /// Check every secret in this value against the declared backends.
    pub fn into_resolved(
        self,
        secrets: &SecretsSection,
        site: impl Fn() -> VarSite,
    ) -> Result<EnvValue, SecretVarError> {
        Ok(match self {
            EnvValue::Scalar(v) => EnvValue::Scalar(v.into_resolved(secrets, &site)?),
            EnvValue::Composed(segs) => EnvValue::Composed(
                segs.into_iter()
                    .map(|s| s.into_resolved(secrets, &site))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        })
    }
}

impl EnvValue {
    pub fn resolve(&self, fetched: &FetchedSecrets) -> Result<String, VarResolveError> {
        self.resolve_with_env(fetched, &|name| std::env::var(name).ok())
    }

    pub(super) fn resolve_with_env(
        &self,
        fetched: &FetchedSecrets,
        environment: &impl Fn(&str) -> Option<String>,
    ) -> Result<String, VarResolveError> {
        match self {
            EnvValue::Scalar(v) => v.resolve_with_env(fetched, environment),
            EnvValue::Composed(segs) => {
                let mut out = String::new();
                for s in segs {
                    out.push_str(&s.resolve_with_env(fetched, environment)?);
                }
                Ok(out)
            }
        }
    }
}

/// A reference to a named entry in the server's `vars` table.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VarRef {
    pub var: String,
}

/// One piece of a `VarTemplate`: a literal string or a `{ var = "name" }` reference.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Segment {
    Literal(String),
    Ref(VarRef),
}

impl Segment {
    pub fn resolve(&self, vars: &HashMap<String, String>) -> Result<String, VarResolveError> {
        match self {
            Segment::Literal(s) => Ok(s.clone()),
            Segment::Ref(VarRef { var }) => vars
                .get(var)
                .cloned()
                .ok_or_else(|| VarResolveError::UndefinedVar(var.clone())),
        }
    }
}

/// A value declaration for `url` or a header.
///
/// Accepts either a single `Segment` (a TOML string or a `{ var = "name" }` table) or
/// an array of segments to be concatenated in order. Inline `{ env = "..." }` is not
/// accepted here — declare it in `vars` first and reference it by name.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum VarTemplate {
    Segments(Vec<Segment>),
    Single(Segment),
}

impl VarTemplate {
    pub fn resolve(&self, vars: &HashMap<String, String>) -> Result<String, VarResolveError> {
        match self {
            VarTemplate::Single(s) => s.resolve(vars),
            VarTemplate::Segments(segs) => {
                let mut out = String::new();
                for s in segs {
                    out.push_str(&s.resolve(vars)?);
                }
                Ok(out)
            }
        }
    }
}

#[cfg(test)]
#[path = "var_tests.rs"]
mod tests;
