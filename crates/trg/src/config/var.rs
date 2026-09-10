use std::collections::HashMap;
use std::fmt;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use crate::secrets::onepassword::{OnePasswordReference, OpReferenceError};
use crate::secrets::{
    BackendKind, KeyError, KeychainReference, OpenbaoReference, PathError, SecretAddress, SecretKey, SecretPath,
    SecretsSection,
};

/// A secret declaration exactly as the file spelled it, before anything knows
/// which backend it names.
///
/// Every addressing field is optional here and none is validated, because
/// which of them is required depends on the kind of the backend `backend`
/// names, and that is not known until the whole document has been read: the
/// `[secrets.backends]` table may well be written below the server that uses
/// it. Resolution into a [`SecretVar`] is the second phase, and it is where
/// every "you addressed this the wrong way" error comes from.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct RawSecretVar {
    /// The `[secrets.backends.<name>]` entry to read from, named here rather
    /// than inherited from the server, so a var says where it comes from
    /// without the reader tracing it through anything.
    pub backend: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    /// Spelled `ref` in the file, which is a Rust keyword.
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
}

impl RawSecretVar {
    /// A declaration addressed by path and key.
    ///
    /// For callers that only know that shape, such as the `trg secret` CLI's
    /// `--path`/`--key` flags.
    pub fn path_key(backend: String, path: &SecretPath, key: &SecretKey) -> Self {
        Self {
            backend,
            path: Some(path.as_str().to_string()),
            key: Some(key.as_str().to_string()),
            reference: None,
        }
    }

    /// The inline table that declares this var in a `vars` table, so the
    /// address just written and the address that will be read cannot be
    /// spelled differently.
    ///
    /// Escaped because a path may hold a character that would otherwise end
    /// the TOML string early.
    pub fn declaration(&self) -> String {
        let mut out = format!("{{ backend = {}", toml_basic_string(&self.backend));
        for (field, value) in [
            ("path", self.path.as_deref()),
            ("key", self.key.as_deref()),
            ("ref", self.reference.as_deref()),
        ] {
            if let Some(value) = value {
                out.push_str(&format!(", {field} = {}", toml_basic_string(value)));
            }
        }
        out.push_str(" }");
        out
    }
}

impl fmt::Display for RawSecretVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "backend `{}`", self.backend)?;
        match (&self.reference, &self.path, &self.key) {
            (Some(reference), _, _) => write!(f, " at `{reference}`"),
            (None, Some(path), Some(key)) => write!(f, " at `{path}`, key `{key}`"),
            (None, Some(path), None) => write!(f, " at `{path}`"),
            (None, None, _) => Ok(()),
        }
    }
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

    fn path_key(&self) -> &'static str {
        match self {
            Self::Table { .. } => "`path`/`key`",
            Self::Flags => "`--path`/`--key`",
        }
    }

    fn path_and_key(&self) -> &'static str {
        match self {
            Self::Table { .. } => "`path` and `key`",
            Self::Flags => "`--path` and `--key`",
        }
    }

    fn reference(&self) -> &'static str {
        match self {
            Self::Table { .. } => "`ref`",
            Self::Flags => "`--ref`",
        }
    }

    fn reference_fix(&self, backend: &str) -> String {
        match self {
            Self::Table { .. } => {
                format!(r#"replace with: {{ backend = "{backend}", ref = "op://<vault>/<item>/<field>" }}"#)
            }
            Self::Flags => r#"use instead: --ref "op://<vault>/<item>/<field>""#.to_string(),
        }
    }

    fn path_key_fix(&self, backend: &str) -> String {
        match self {
            Self::Table { .. } => format!(r#"replace with: {{ backend = "{backend}", path = "...", key = "..." }}"#),
            Self::Flags => "use instead: --path <path> --key <key>".to_string(),
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

    /// The replacement is spelled out because this is the whole migration
    /// path off the old `{ path, key }` spelling: there is deliberately no
    /// compatibility bridge, so the error has to be the instructions.
    #[error(
        "{site} names backend `{backend}`, which is addressed with a 1Password secret reference, not {}\n  {}",
        .site.path_key(),
        .site.reference_fix(.backend)
    )]
    NeedsReference { site: VarSite, backend: String },

    #[error(
        "{site} names backend `{backend}` of kind `{kind}`, which is addressed with {}, not {}\n  {}",
        .site.path_and_key(),
        .site.reference(),
        .site.path_key_fix(.backend)
    )]
    RefNotSupported {
        site: VarSite,
        backend: String,
        kind: BackendKind,
    },

    #[error(
        "{site} names backend `{backend}` of kind `{kind}`, which needs both `path` and `key`; `{missing}` is missing"
    )]
    Incomplete {
        site: VarSite,
        backend: String,
        kind: BackendKind,
        missing: &'static str,
    },

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
        match &self.address {
            SecretAddress::Keychain(r) => RawSecretVar::path_key(self.backend.clone(), r.path(), r.key()),
            SecretAddress::Openbao(r) => RawSecretVar::path_key(self.backend.clone(), r.path(), r.key()),
            SecretAddress::OnePassword(r) => RawSecretVar {
                backend: self.backend.clone(),
                path: None,
                key: None,
                reference: Some(r.to_string()),
            },
        }
    }
}

/// Read a raw declaration in the vocabulary of one backend kind.
///
/// Split out of [`SecretVar::resolve`] so the dispatch on kind reads as the
/// one place that says how each backend is addressed. A kind added to
/// [`BackendKind`] cannot be forgotten here.
fn parse_address(kind: BackendKind, raw: &RawSecretVar, site: &VarSite) -> Result<SecretAddress, SecretVarError> {
    match kind {
        BackendKind::Keychain | BackendKind::Openbao => {
            if raw.reference.is_some() {
                return Err(SecretVarError::RefNotSupported {
                    site: site.clone(),
                    backend: raw.backend.clone(),
                    kind,
                });
            }
            let missing = match (&raw.path, &raw.key) {
                (None, _) => Some("path"),
                (_, None) => Some("key"),
                _ => None,
            };
            if let Some(missing) = missing {
                return Err(SecretVarError::Incomplete {
                    site: site.clone(),
                    backend: raw.backend.clone(),
                    kind,
                    missing,
                });
            }
            let (Some(path), Some(key)) = (&raw.path, &raw.key) else {
                unreachable!("both are present once `missing` is None")
            };
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
        BackendKind::OnePassword => {
            if raw.path.is_some() || raw.key.is_some() || raw.reference.is_none() {
                return Err(SecretVarError::NeedsReference {
                    site: site.clone(),
                    backend: raw.backend.clone(),
                });
            }
            let raw_ref = raw.reference.as_deref().unwrap_or_default();
            OnePasswordReference::parse(raw_ref)
                .map(|r| SecretAddress::OnePassword(Box::new(r)))
                .map_err(|cause| SecretVarError::Reference {
                    site: site.clone(),
                    cause,
                })
        }
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
pub struct FetchedSecrets(HashMap<SecretVar, SecretString>);

impl FetchedSecrets {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, var: SecretVar, value: SecretString) {
        self.0.insert(var, value);
    }

    pub fn get(&self, var: &SecretVar) -> Option<&SecretString> {
        self.0.get(var)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A value source for `[mcp.servers.<name>.vars]` entries.
///
/// `Literal` is a bare TOML string; `Env` is an inline `{ env, default? }`
/// table; `Secret` is an inline secret declaration, whose addressing fields
/// depend on the backend it names.
/// `VarSource` is intentionally accepted only inside a `vars` table, never directly
/// in `url` or header values.
///
/// Generic over the secret payload rather than holding a half-filled
/// [`SecretVar`], so that "read from the file, backend not yet known" and
/// "checked against the backend it names" are different types and a resolved
/// tree cannot be built by forgetting a step.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
#[serde(bound(deserialize = "S: Deserialize<'de>"))]
#[serde(expecting = "a string, or an inline table `{ env = \"NAME\", default = \"...\" }`, \
                     `{ backend = \"NAME\", path = \"...\", key = \"...\" }` \
                     or `{ backend = \"NAME\", ref = \"op://<vault>/<item>/<field>\" }`")]
pub enum VarSource<S = SecretVar> {
    Literal(String),
    Env {
        env: String,
        #[serde(default)]
        default: Option<String>,
    },
    Secret(S),
}

/// A [`VarSource`] as deserialised, before its backend is known.
pub type RawVarSource = VarSource<RawSecretVar>;

impl<S> VarSource<S> {
    /// The backend read this source needs before it can resolve, if any.
    pub fn secret(&self) -> Option<&S> {
        match self {
            VarSource::Secret(v) => Some(v),
            VarSource::Literal(_) | VarSource::Env { .. } => None,
        }
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
        }
    }
}

impl VarSource {
    pub fn resolve(&self, fetched: &FetchedSecrets) -> Result<String, VarResolveError> {
        match self {
            VarSource::Literal(s) => Ok(s.clone()),
            VarSource::Env { env, default } => resolve_env(env, default.as_deref()),
            // Absent here means the caller resolved without fetching first,
            // which is a wiring mistake rather than anything the config said.
            VarSource::Secret(v) => fetched
                .get(v)
                .map(|s| s.expose_secret().to_string())
                .ok_or_else(|| VarResolveError::SecretNotFetched(v.clone())),
        }
    }
}

fn resolve_env(name: &str, default: Option<&str>) -> Result<String, VarResolveError> {
    match std::env::var(name) {
        Ok(v) => Ok(v),
        Err(_) => default
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
    SecretNotFetched(SecretVar),

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
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged, bound(deserialize = "S: Deserialize<'de>"))]
pub enum EnvValue<S = SecretVar> {
    Scalar(VarSource<S>),
    Composed(Vec<VarSource<S>>),
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
        match self {
            EnvValue::Scalar(v) => v.resolve(fetched),
            EnvValue::Composed(segs) => {
                let mut out = String::new();
                for s in segs {
                    out.push_str(&s.resolve(fetched)?);
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
mod tests {
    use super::*;

    /// A `[secrets]` section declaring the given backends, so a var can be
    /// resolved against something that says what kind each name is.
    fn declaring(backends: &[(&str, &str)]) -> SecretsSection {
        let mut text = String::new();
        for (name, kind) in backends {
            text.push_str(&format!("[backends.{name}]\nkind = \"{kind}\"\n"));
            if *kind == "onepassword" {
                text.push_str("account = \"my.1password.com\"\n");
            }
            if *kind == "openbao" {
                text.push_str(
                    "addr = \"https://bao.example\"\nmount = \"kv\"\npath_prefix = \"trg\"\nowner = \"me\"\ntoken_file = \"~/.t\"\n",
                );
            }
        }
        let section: SecretsSection = toml::from_str(&text).expect("test section");
        assert_eq!(section.backends.len(), backends.len());
        section
    }

    fn raw(table: &str) -> RawSecretVar {
        #[derive(Deserialize)]
        struct W {
            t: RawSecretVar,
        }
        toml::from_str::<W>(&format!("t = {table}")).expect("raw var").t
    }

    fn resolve(table: &str, backends: &[(&str, &str)]) -> Result<SecretVar, SecretVarError> {
        SecretVar::resolve(&raw(table), &declaring(backends), VarSite::exec_env("acme", "TOKEN"))
    }

    fn keychain_var() -> SecretVar {
        resolve(
            r#"{ backend = "login", path = "agentgateway", key = "token" }"#,
            &[("login", "keychain")],
        )
        .expect("resolves")
    }

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn varsource_literal_resolves() {
        assert_eq!(
            VarSource::Literal("x".into()).resolve(&FetchedSecrets::new()).unwrap(),
            "x"
        );
    }

    #[test]
    fn varsource_env_resolves() {
        let name = format!("TRG_VARSRC_ENV_{}", std::process::id());
        std::env::set_var(&name, "hi");
        let e = VarSource::Env {
            env: name.clone(),
            default: None,
        };
        assert_eq!(e.resolve(&FetchedSecrets::new()).unwrap(), "hi");
        std::env::remove_var(&name);
    }

    #[test]
    fn varsource_env_falls_back_to_default() {
        let name = format!("TRG_VARSRC_UNSET_{}", std::process::id());
        std::env::remove_var(&name);
        let e = VarSource::Env {
            env: name,
            default: Some("d".into()),
        };
        assert_eq!(e.resolve(&FetchedSecrets::new()).unwrap(), "d");
    }

    #[test]
    fn varsource_env_missing_without_default_errors() {
        let name = format!("TRG_VARSRC_REQ_{}", std::process::id());
        std::env::remove_var(&name);
        let e = VarSource::Env {
            env: name.clone(),
            default: None,
        };
        let err = e.resolve(&FetchedSecrets::new()).unwrap_err();
        assert!(matches!(err, VarResolveError::MissingEnv(ref n) if n == &name));
    }

    #[test]
    fn varsource_deserializes_literal_and_env() {
        let mut m: HashMap<String, RawVarSource> = toml::from_str(
            r#"
a = "literal"
b = { env = "X", default = "d" }
c = { env = "REQ" }
"#,
        )
        .unwrap();
        assert!(matches!(m.remove("a").unwrap(), VarSource::Literal(_)));
        assert!(matches!(m.remove("b").unwrap(), VarSource::Env { .. }));
        assert!(matches!(m.remove("c").unwrap(), VarSource::Env { .. }));
    }

    #[test]
    fn varsource_deserializes_a_secret_table() {
        let m: HashMap<String, RawVarSource> =
            toml::from_str(r#"t = { backend = "homelab", path = "agentgateway", key = "token" }"#).unwrap();
        let want = raw(r#"{ backend = "homelab", path = "agentgateway", key = "token" }"#);
        assert_eq!(m["t"].secret(), Some(&want));
    }

    #[test]
    fn varsource_deserializes_a_reference_table() {
        let m: HashMap<String, RawVarSource> =
            toml::from_str(r#"t = { backend = "op", ref = "op://Ops/deploy/TOKEN" }"#).unwrap();
        assert_eq!(
            m["t"].secret().and_then(|v| v.reference.as_deref()),
            Some("op://Ops/deploy/TOKEN")
        );
    }

    /// The address is the secret's whole identity, so two declarations of the
    /// same secret are the same key no matter which server wrote them.
    #[test]
    fn two_declarations_of_one_address_are_one_key() {
        let m: HashMap<String, RawVarSource> = toml::from_str(
            r#"
a = { backend = "homelab", path = "agentgateway", key = "token" }
b = { backend = "homelab", path = "agentgateway", key = "token" }
"#,
        )
        .unwrap();
        assert_eq!(m["a"].secret(), m["b"].secret());
    }

    #[test]
    fn varsource_secret_resolves_from_what_was_fetched() {
        let var = keychain_var();
        let mut fetched = FetchedSecrets::new();
        fetched.insert(var.clone(), SecretString::from("t".to_string()));

        assert_eq!(VarSource::Secret(var).resolve(&fetched).unwrap(), "t");
    }

    /// Resolving before fetching is the caller's mistake, and the message has
    /// to say so rather than blaming the config for a value it declared fine.
    #[test]
    fn varsource_secret_without_a_fetch_is_reported_as_such() {
        let err = VarSource::Secret(keychain_var())
            .resolve(&FetchedSecrets::new())
            .unwrap_err();
        assert!(matches!(err, VarResolveError::SecretNotFetched(_)), "{err}");
    }

    #[test]
    fn varsource_rejects_unknown_field() {
        let err = toml::from_str::<RawVarSource>(r#"{ env = "E", typo = true }"#).unwrap_err();
        assert!(format!("{}", err).contains("typo"));
    }

    #[test]
    fn template_single_literal_resolves() {
        #[derive(Deserialize)]
        struct W {
            v: VarTemplate,
        }
        let w: W = toml::from_str(r#"v = "hello""#).unwrap();
        assert_eq!(w.v.resolve(&HashMap::new()).unwrap(), "hello");
    }

    #[test]
    fn template_single_varref_resolves() {
        #[derive(Deserialize)]
        struct W {
            v: VarTemplate,
        }
        let w: W = toml::from_str(r#"v = { var = "host" }"#).unwrap();
        assert_eq!(w.v.resolve(&vars(&[("host", "example.com")])).unwrap(), "example.com");
    }

    #[test]
    fn template_segments_concatenate() {
        #[derive(Deserialize)]
        struct W {
            v: VarTemplate,
        }
        let w: W = toml::from_str(r#"v = ["https://", { var = "host" }, "/x"]"#).unwrap();
        assert_eq!(
            w.v.resolve(&vars(&[("host", "example.com")])).unwrap(),
            "https://example.com/x",
        );
    }

    #[test]
    fn template_empty_segments_resolves_to_empty_string() {
        #[derive(Deserialize)]
        struct W {
            v: VarTemplate,
        }
        let w: W = toml::from_str("v = []").unwrap();
        assert_eq!(w.v.resolve(&HashMap::new()).unwrap(), "");
    }

    /// The `v = ...` wrapper a bare `EnvValue` needs to be TOML at all,
    /// resolved against the backends `declaring` set up.
    fn env_value(toml_text: &str, backends: &[(&str, &str)]) -> EnvValue {
        #[derive(Deserialize)]
        struct W {
            v: RawEnvValue,
        }
        toml::from_str::<W>(toml_text)
            .expect("env value")
            .v
            .into_resolved(&declaring(backends), || VarSite::exec_env("acme", "V"))
            .expect("resolves")
    }

    #[test]
    fn envvalue_composed_literal_and_env_segments_concatenate() {
        let name = format!("TRG_ENVVALUE_ENV_{}", std::process::id());
        std::env::set_var(&name, "/home/tester");
        let v = env_value(&format!(r#"v = [{{ env = "{name}" }}, "/app/state"]"#), &[]);
        assert_eq!(v.resolve(&FetchedSecrets::new()).unwrap(), "/home/tester/app/state");
        std::env::remove_var(&name);
    }

    #[test]
    fn envvalue_composed_secret_segment_is_collected_and_resolved() {
        let v = env_value(
            r#"v = ["prefix-", { backend = "vault", path = "Ops/deploy-keys", key = "TOKEN" }]"#,
            &[("vault", "keychain")],
        );
        let want = v.secrets().first().copied().expect("one secret").clone();

        let mut fetched = FetchedSecrets::new();
        fetched.insert(want, SecretString::from("secretvalue".to_string()));
        assert_eq!(v.resolve(&fetched).unwrap(), "prefix-secretvalue");
    }

    #[test]
    fn envvalue_empty_composed_resolves_to_empty_string() {
        assert_eq!(env_value("v = []", &[]).resolve(&FetchedSecrets::new()).unwrap(), "");
    }

    #[test]
    fn envvalue_scalar_behaves_like_a_bare_varsource() {
        let v = env_value(r#"v = "plain""#, &[]);
        assert!(v.secrets().is_empty());
        assert_eq!(v.resolve(&FetchedSecrets::new()).unwrap(), "plain");

        let name = format!("TRG_ENVVALUE_SCALAR_ENV_{}", std::process::id());
        std::env::set_var(&name, "hi");
        let v = env_value(&format!(r#"v = {{ env = "{name}" }}"#), &[]);
        assert_eq!(v.resolve(&FetchedSecrets::new()).unwrap(), "hi");
        std::env::remove_var(&name);

        let v = env_value(
            r#"v = { backend = "op", ref = "op://Ops/deploy-keys/TOKEN" }"#,
            &[("op", "onepassword")],
        );
        let want = v.secrets().first().copied().expect("one secret").clone();
        let mut fetched = FetchedSecrets::new();
        fetched.insert(want, SecretString::from("s".to_string()));
        assert_eq!(v.resolve(&fetched).unwrap(), "s");
    }

    #[test]
    fn template_undefined_var_errors() {
        #[derive(Deserialize)]
        struct W {
            v: VarTemplate,
        }
        let w: W = toml::from_str(r#"v = { var = "missing" }"#).unwrap();
        let err = w.v.resolve(&HashMap::new()).unwrap_err();
        assert!(matches!(err, VarResolveError::UndefinedVar(ref n) if n == "missing"));
    }

    #[test]
    fn template_rejects_inline_env_in_url_position() {
        #[derive(Debug, Deserialize)]
        #[allow(dead_code)]
        struct W {
            v: VarTemplate,
        }
        let err = toml::from_str::<W>(r#"v = { env = "X" }"#).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("did not match") || msg.contains("unknown field"),
            "got: {msg}"
        );
    }

    #[test]
    fn template_rejects_inline_env_inside_segments() {
        #[derive(Debug, Deserialize)]
        #[allow(dead_code)]
        struct W {
            v: VarTemplate,
        }
        let err = toml::from_str::<W>(r#"v = ["x", { env = "Y" }, "z"]"#).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("did not match") || msg.contains("unknown field"),
            "got: {msg}"
        );
    }

    /// Built straight from an address rather than through TOML, so a path
    /// carrying a character TOML would have to escape can be exercised.
    fn keychain(backend: &str, path: &str, key: &str) -> SecretVar {
        SecretVar::new(
            backend.to_string(),
            SecretAddress::Keychain(KeychainReference::new(
                SecretPath::parse(path).expect("path"),
                SecretKey::parse(key).expect("key"),
            )),
        )
    }

    /// A path may hold a space, so a command offered as the fix has to survive
    /// being pasted into a shell.
    #[test]
    fn a_put_command_quotes_what_a_shell_would_otherwise_split() {
        assert_eq!(
            keychain("home lab", "mcp/a b", "token").put_command().as_deref(),
            Some("trg secret put --backend 'home lab' --path 'mcp/a b' --key token")
        );
    }

    #[test]
    fn a_plain_put_command_is_left_unquoted() {
        assert_eq!(
            keychain("homelab", "mcp/memorizer", "token").put_command().as_deref(),
            Some("trg secret put --backend homelab --path mcp/memorizer --key token")
        );
    }

    /// 1Password items are managed in 1Password, so an error about a missing
    /// one must not send someone to a command that would only fail.
    #[test]
    fn a_read_only_backend_offers_no_put_command() {
        let var = resolve(
            r#"{ backend = "op", ref = "op://Ops/deploy/TOKEN" }"#,
            &[("op", "onepassword")],
        )
        .expect("resolves");
        assert_eq!(var.put_command(), None);
    }

    /// The declaration is meant to be pasted back into `vars`, so it has to
    /// parse as the table it claims to be.
    #[test]
    fn a_declaration_survives_a_quote_in_a_path() {
        let var = keychain("homelab", r#"mcp/a"b\c"#, "token");
        let toml_text = format!("[vars]\ntoken = {}\n", var.declaration());
        let parsed: toml::Value = toml::from_str(&toml_text).expect("declaration must parse");
        assert_eq!(parsed["vars"]["token"]["path"].as_str().unwrap(), r#"mcp/a"b\c"#);
    }

    #[test]
    fn a_declaration_keeps_a_newline_on_one_line() {
        let line = keychain("homelab", "a\nb", "token").declaration();
        assert!(!line.contains('\n'), "{line}");
        let parsed: toml::Value = toml::from_str(&format!("[vars]\ntoken = {line}\n")).expect("parse");
        assert_eq!(parsed["vars"]["token"]["path"].as_str().unwrap(), "a\nb");
    }

    /// A 1Password var declares itself the way 1Password addresses it, so the
    /// line offered here is the line that loads.
    #[test]
    fn a_reference_declares_itself_as_a_reference() {
        let var = resolve(
            r#"{ backend = "op", ref = "op://Ops/deploy/section/TOKEN" }"#,
            &[("op", "onepassword")],
        )
        .expect("resolves");
        assert_eq!(
            var.declaration(),
            r#"{ backend = "op", ref = "op://Ops/deploy/section/TOKEN" }"#
        );
    }

    #[test]
    fn each_backend_kind_accepts_the_address_it_is_written_with() {
        let keychain = resolve(
            r#"{ backend = "login", path = "agentgateway", key = "token" }"#,
            &[("login", "keychain")],
        )
        .expect("keychain resolves");
        assert!(matches!(keychain.address(), SecretAddress::Keychain(_)));

        let openbao = resolve(
            r#"{ backend = "homelab", path = "agentgateway", key = "token" }"#,
            &[("homelab", "openbao")],
        )
        .expect("openbao resolves");
        assert!(matches!(openbao.address(), SecretAddress::Openbao(_)));

        let onepassword = resolve(
            r#"{ backend = "op", ref = "op://Ops/deploy/TOKEN" }"#,
            &[("op", "onepassword")],
        )
        .expect("onepassword resolves");
        assert!(matches!(onepassword.address(), SecretAddress::OnePassword(_)));
    }

    /// The whole point of resolving against the declared kind: a var written
    /// in the wrong vocabulary is a config error, and the error is the
    /// migration path off the old spelling.
    #[test]
    fn a_path_and_key_address_is_refused_for_onepassword() {
        let err = resolve(
            r#"{ backend = "op", path = "Ops/deploy", key = "TOKEN" }"#,
            &[("op", "onepassword")],
        )
        .expect_err("path and key is not how 1Password is addressed");
        let message = err.to_string();
        assert!(message.contains("[exec.acme.env] TOKEN"), "{message}");
        assert!(
            message.contains("1Password secret reference, not `path`/`key`"),
            "{message}"
        );
        assert!(
            message.contains(r#"replace with: { backend = "op", ref = "op://<vault>/<item>/<field>" }"#),
            "{message}"
        );
    }

    #[test]
    fn a_reference_is_refused_for_a_path_and_key_backend() {
        for (name, kind) in [("login", "keychain"), ("homelab", "openbao")] {
            let err = resolve(
                &format!(r#"{{ backend = "{name}", ref = "op://Ops/deploy/TOKEN" }}"#),
                &[(name, kind)],
            )
            .expect_err("ref is not how this backend is addressed");
            let message = err.to_string();
            assert!(message.contains(&format!("of kind `{kind}`")), "{message}");
            assert!(message.contains("`path` and `key`, not `ref`"), "{message}");
        }
    }

    #[test]
    fn a_half_spelled_path_and_key_address_names_what_is_missing() {
        let err = resolve(
            r#"{ backend = "login", path = "agentgateway" }"#,
            &[("login", "keychain")],
        )
        .expect_err("key is required");
        assert!(err.to_string().contains("`key` is missing"), "{err}");
    }

    #[test]
    fn a_reference_is_required_for_onepassword() {
        let err = resolve(r#"{ backend = "op" }"#, &[("op", "onepassword")]).expect_err("ref is required");
        assert!(err.to_string().contains("not `path`/`key`"), "{err}");
    }

    /// Nothing checked this before, so a typo in a backend name surfaced as a
    /// failed read at launch time rather than as the config mistake it is.
    #[test]
    fn a_var_naming_an_undeclared_backend_lists_the_declared_ones() {
        let err = resolve(
            r#"{ backend = "hoemlab", path = "a", key = "k" }"#,
            &[("homelab", "openbao"), ("login", "keychain")],
        )
        .expect_err("no such backend");
        let message = err.to_string();
        assert!(
            message.contains("names backend `hoemlab`, which is not declared"),
            "{message}"
        );
        assert!(message.contains("declared: homelab, login"), "{message}");
    }

    /// Listing nothing would read as "the name is wrong" when the real
    /// mistake is that no backend has been declared at all.
    #[test]
    fn a_var_with_no_backends_at_all_says_to_declare_one() {
        let err = resolve(r#"{ backend = "homelab", path = "a", key = "k" }"#, &[]).expect_err("nothing declared");
        assert!(
            err.to_string().contains("add a `[secrets.backends.homelab]` section"),
            "{err}"
        );
    }

    #[test]
    fn a_malformed_reference_is_a_config_error_naming_the_site() {
        let err = resolve(
            r#"{ backend = "op", ref = "Ops/deploy/TOKEN" }"#,
            &[("op", "onepassword")],
        )
        .expect_err("no scheme");
        let message = err.to_string();
        assert!(message.contains("[exec.acme.env] TOKEN"), "{message}");
        assert!(message.contains("must start with `op://`"), "{message}");
    }
}
