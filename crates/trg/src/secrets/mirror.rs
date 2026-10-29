//! One-way mirroring of a value from one backend into another.
//!
//! The source stays the source of truth: a mirror copies a value toward a
//! target `trg` can write to, and never reads the target back as an input.
//! That is also why a mirror is a declaration rather than a flag on
//! `trg secret put`. The pair is what is worth keeping, so that running it
//! again converges on the same state instead of depending on whoever typed
//! the pipeline last.
//!
//! Whether two values agree is decided by a [`Fingerprint`], never by
//! comparing or printing the values themselves.

use std::collections::HashMap;
use std::fmt;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracing::field::Empty;
use tracing::Instrument;

use super::telemetry::record_error;
use super::vars::{self, VarFetchError};
use super::{Backend, BackendError, BackendKind, SecretAddress, SecretKey, SecretPath, SecretsError, SecretsSection};
use crate::config::{RawSecretVar, SecretVar, SecretVarError, VarSite};
use crate::telemetry::semconv::trg::SECRETS_BACKEND_NAME;

/// The key written next to the value, naming where it came from.
const SOURCE_KEY: &str = "source";

/// `<byte length>:<sha256>` of a value with one trailing newline removed.
///
/// One newline comes off because a backend may add one on the way in, and a
/// value that differs only by that is the same value. The length leads so an
/// empty value can never be mistaken for an agreeing one: its digest is a
/// constant, but its length is not a length any real secret has.
#[derive(Clone, PartialEq, Eq)]
pub struct Fingerprint {
    length: usize,
    digest: [u8; 32],
}

impl Fingerprint {
    pub fn of(value: &str) -> Self {
        let value = value.strip_suffix('\n').unwrap_or(value);
        Self {
            length: value.len(),
            digest: Sha256::digest(value.as_bytes()).into(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.length, crate::agentskills::hex_encode(self.digest))
    }
}

/// Redacted to the length, because a digest of a weak secret is a way to
/// guess it and this type reaches `Debug` through the error types.
impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({} bytes)", self.length)
    }
}

/// The name a `[secrets.mirrors.<name>]` table is declared under.
///
/// Limited to what a TOML bare key holds, so a mirror can always be written
/// as `[secrets.mirrors.<name>]` without quoting and can be typed on the
/// command line as it is written in the file.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "String")]
pub struct MirrorName(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MirrorNameError {
    #[error("a mirror name must not be empty")]
    Empty,

    #[error("mirror name `{0}` may hold only ASCII letters, digits, `-` and `_`")]
    Characters(String),
}

impl MirrorName {
    pub fn parse(raw: &str) -> Result<Self, MirrorNameError> {
        if raw.is_empty() {
            return Err(MirrorNameError::Empty);
        }
        if !raw.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) {
            return Err(MirrorNameError::Characters(raw.to_string()));
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MirrorName {
    type Error = MirrorNameError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(&raw)
    }
}

impl fmt::Display for MirrorName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One `[secrets.mirrors.<name>]` table as the file spelled it.
///
/// Every side is the inline table a var is written as, so a value is
/// addressed here exactly as it is anywhere else in the config.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MirrorConfig {
    pub source: RawSecretVar,
    pub targets: Vec<RawSecretVar>,
}

/// One place a mirror writes, already checked to be one `trg` can write to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MirrorTarget {
    var: SecretVar,
    path: SecretPath,
    key: SecretKey,
}

impl MirrorTarget {
    pub fn var(&self) -> &SecretVar {
        &self.var
    }

    /// `<path>#<key> (<backend>)`, for a line of output that already says
    /// which mirror it is about.
    pub fn describe(&self) -> String {
        format!("{}#{} ({})", self.path, self.key, self.var.backend())
    }
}

/// A mirror whose sides have been checked against the declared backends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mirror {
    name: MirrorName,
    source: SecretVar,
    targets: Vec<MirrorTarget>,
}

#[derive(Debug, thiserror::Error)]
pub enum MirrorConfigError {
    #[error("{0}")]
    Var(#[from] SecretVarError),

    #[error("`[secrets.mirrors.{name}]`: `targets` must name at least one target")]
    NoTargets { name: MirrorName },

    #[error(
        "`[secrets.mirrors.{name}]`: target backend `{backend}` is of kind `{kind}`, which `trg` only reads from; mirror into a `keychain` or `openbao` backend"
    )]
    TargetNotWritable {
        name: MirrorName,
        backend: String,
        kind: BackendKind,
    },

    #[error("`[secrets.mirrors.{name}]`: the source and a target are the same address, {address}")]
    SameAddress { name: MirrorName, address: String },

    #[error(
        "`[secrets.mirrors.{name}]`: target key `{SOURCE_KEY}` is reserved for the reference to where the value came from"
    )]
    ReservedKey { name: MirrorName },

    #[error("`[secrets.mirrors.{name}]`: target {target} is listed more than once")]
    RepeatedTarget { name: MirrorName, target: String },

    #[error("`[secrets.mirrors.{name}]`: target {target} is already a target of `[secrets.mirrors.{first}]`")]
    DuplicateTarget {
        name: MirrorName,
        first: MirrorName,
        target: String,
    },

    #[error(
        "`[secrets.mirrors.{name}]`: path `{path}` in backend `{backend}` is also written by `[secrets.mirrors.{first}]` from a different source; the `{SOURCE_KEY}` key there records one origin, so mirrors sharing a path must share a source"
    )]
    ConflictingSource {
        name: MirrorName,
        first: MirrorName,
        backend: String,
        path: String,
    },
}

impl Mirror {
    pub fn name(&self) -> &MirrorName {
        &self.name
    }

    pub fn source(&self) -> &SecretVar {
        &self.source
    }

    pub fn targets(&self) -> &[MirrorTarget] {
        &self.targets
    }

    /// Where the value came from, as written into each target's `source` key.
    ///
    /// A 1Password reference already is a portable pointer. The other kinds
    /// are addressed by backend name, path and key, so the name is part of it.
    fn source_reference(&self) -> String {
        match self.source.address() {
            SecretAddress::OnePassword(r) => r.to_string(),
            SecretAddress::Keychain(r) => format!("{}:{}#{}", self.source.backend(), r.path(), r.key()),
            SecretAddress::Openbao(r) => format!("{}:{}#{}", self.source.backend(), r.path(), r.key()),
        }
    }
}

/// Resolve every declared mirror, in name order, so output and errors do not
/// depend on how the file happened to be arranged.
pub fn resolve_all(secrets: &SecretsSection) -> Result<Vec<Mirror>, MirrorConfigError> {
    let mut out = Vec::with_capacity(secrets.mirrors.len());
    let mut claimed: HashMap<SecretVar, MirrorName> = HashMap::new();
    let mut paths: HashMap<(String, SecretPath), (MirrorName, SecretVar)> = HashMap::new();

    for (name, config) in &secrets.mirrors {
        let mirror = resolve_one(name, config, secrets)?;
        for target in &mirror.targets {
            match claimed.get(&target.var) {
                Some(first) if first == name => {
                    return Err(MirrorConfigError::RepeatedTarget {
                        name: name.clone(),
                        target: target.var.to_string(),
                    })
                }
                Some(first) => {
                    return Err(MirrorConfigError::DuplicateTarget {
                        name: name.clone(),
                        first: first.clone(),
                        target: target.var.to_string(),
                    })
                }
                None => {
                    claimed.insert(target.var.clone(), name.clone());
                }
            }
            let at = (target.var.backend().to_string(), target.path.clone());
            match paths.get(&at) {
                Some((first, source)) if source != &mirror.source => {
                    return Err(MirrorConfigError::ConflictingSource {
                        name: name.clone(),
                        first: first.clone(),
                        backend: at.0,
                        path: at.1.to_string(),
                    })
                }
                Some(_) => {}
                None => {
                    paths.insert(at, (name.clone(), mirror.source.clone()));
                }
            }
        }
        out.push(mirror);
    }

    Ok(out)
}

fn resolve_one(
    name: &MirrorName,
    config: &MirrorConfig,
    secrets: &SecretsSection,
) -> Result<Mirror, MirrorConfigError> {
    if config.targets.is_empty() {
        return Err(MirrorConfigError::NoTargets { name: name.clone() });
    }
    let source = SecretVar::resolve(&config.source, secrets, VarSite::mirror(name.as_str(), "source"))?;

    let mut targets = Vec::with_capacity(config.targets.len());
    for (offset, raw) in config.targets.iter().enumerate() {
        let side = format!("targets[{offset}]");
        let var = SecretVar::resolve(raw, secrets, VarSite::mirror(name.as_str(), &side))?;

        let kind = var.address().backend_kind();
        if !kind.is_writable() {
            return Err(MirrorConfigError::TargetNotWritable {
                name: name.clone(),
                backend: var.backend().to_string(),
                kind,
            });
        }
        if source == var {
            return Err(MirrorConfigError::SameAddress {
                name: name.clone(),
                address: source.to_string(),
            });
        }
        let Some((path, key)) = var.address().path_key() else {
            unreachable!("a writable kind is addressed by path and key")
        };
        if key.as_str() == SOURCE_KEY {
            return Err(MirrorConfigError::ReservedKey { name: name.clone() });
        }
        let (path, key) = (path.clone(), key.clone());
        targets.push(MirrorTarget { var, path, key });
    }

    Ok(Mirror {
        name: name.clone(),
        source,
        targets,
    })
}

/// What a mirror did, or in a dry run what it would do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MirrorOutcome {
    UpToDate,
    WouldCreate,
    WouldUpdate,
    Created,
    Updated,
}

impl MirrorOutcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::UpToDate => "up to date",
            Self::WouldCreate => "would create",
            Self::WouldUpdate => "would update",
            Self::Created => "created",
            Self::Updated => "updated",
        }
    }

    /// Whether the target held something other than what the source holds,
    /// whether or not this run changed it.
    pub fn is_drift(self) -> bool {
        !matches!(self, Self::UpToDate)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MirrorError {
    #[error("could not read the source: {0}")]
    Source(#[from] VarFetchError),

    #[error("refusing to mirror an empty source value; a source that was emptied or never filled is the usual reason for one")]
    EmptySource,

    #[error("target backend `{name}`: {cause}")]
    TargetBackend {
        name: String,
        #[source]
        cause: Box<BackendError>,
    },

    #[error("could not read the target: {0}")]
    TargetRead(#[source] SecretsError),

    #[error("could not write the target: {0}")]
    TargetWrite(#[source] SecretsError),

    #[error("the target did not read back what was written")]
    ReadbackMismatch,
}

impl MirrorError {
    /// A low-cardinality name for this failure, for `error.type`.
    pub fn error_type(&self) -> &'static str {
        match self {
            Self::Source(e) => e.error_type(),
            Self::EmptySource => "empty_source",
            Self::TargetBackend { .. } => "backend_config",
            Self::TargetRead(e) | Self::TargetWrite(e) => e.error_type(),
            Self::ReadbackMismatch => "readback_mismatch",
        }
    }
}

/// What each target of one mirror did, in the order the mirror lists them.
pub type TargetOutcomes = Vec<Result<MirrorOutcome, MirrorError>>;

/// Bring every target of a mirror in line with its source, or report how it
/// would.
///
/// The source is read once for all of them, and an empty or unreadable source
/// is the one failure that stops every target. After that each target stands
/// alone, so one backend being down does not hide that the others are in
/// sync or still drifted.
///
/// A dry run reads both sides and writes nothing. An applied run writes only
/// when the fingerprints differ, then reads the target back and fails if what
/// it holds is not what was written, since a write the backend acknowledged
/// but did not keep must not be reported as mirrored.
pub async fn mirror<F>(resolve: &F, mirror: &Mirror, apply: bool) -> Result<TargetOutcomes, MirrorError>
where
    F: Fn(&str) -> Result<Backend, BackendError>,
{
    let fetched = vars::fetch_with(resolve, std::slice::from_ref(&mirror.source)).await?;
    let Some(value) = fetched.get(&mirror.source) else {
        unreachable!("a fetch answers for every var it was asked about")
    };
    let wanted = Fingerprint::of(value.expose_secret());
    if wanted.is_empty() {
        return Err(MirrorError::EmptySource);
    }
    let reference = mirror.source_reference();

    let mut outcomes = Vec::with_capacity(mirror.targets.len());
    for target in &mirror.targets {
        let span = tracing::info_span!(
            "mirror target",
            { SECRETS_BACKEND_NAME } = target.var.backend(),
            "error.type" = Empty,
            "otel.status_code" = Empty,
        );
        let outcome = sync_target(resolve, target, value, &wanted, &reference, apply)
            .instrument(span.clone())
            .await;
        if let Err(e) = &outcome {
            record_error(&span, e.error_type());
        }
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

async fn sync_target<F>(
    resolve: &F,
    target: &MirrorTarget,
    value: &SecretString,
    wanted: &Fingerprint,
    reference: &str,
    apply: bool,
) -> Result<MirrorOutcome, MirrorError>
where
    F: Fn(&str) -> Result<Backend, BackendError>,
{
    let backend = resolve(target.var.backend()).map_err(|cause| MirrorError::TargetBackend {
        name: target.var.backend().to_string(),
        cause: Box::new(cause),
    })?;
    let existing = backend.get(&target.path).await.map_err(MirrorError::TargetRead)?;
    let held = existing
        .as_ref()
        .and_then(|map| map.get(&target.key))
        .map(|held| Fingerprint::of(held.expose_secret()));

    let outcome = match &held {
        None => MirrorOutcome::WouldCreate,
        Some(held) if held == wanted => return Ok(MirrorOutcome::UpToDate),
        Some(_) => MirrorOutcome::WouldUpdate,
    };
    if !apply {
        return Ok(outcome);
    }

    let mut map = existing.unwrap_or_default();
    map.insert(target.key.clone(), value.clone());
    let Ok(source_key) = SecretKey::parse(SOURCE_KEY) else {
        unreachable!("`{SOURCE_KEY}` is a valid key")
    };
    map.insert(source_key, SecretString::from(reference.to_string()));
    backend
        .set(&target.path, &map)
        .await
        .map_err(MirrorError::TargetWrite)?;

    verify_readback(&backend, target, wanted).await?;

    Ok(match outcome {
        MirrorOutcome::WouldCreate => MirrorOutcome::Created,
        _ => MirrorOutcome::Updated,
    })
}

async fn verify_readback(backend: &Backend, target: &MirrorTarget, wanted: &Fingerprint) -> Result<(), MirrorError> {
    let back = backend
        .get(&target.path)
        .await
        .map_err(MirrorError::TargetRead)?
        .and_then(|map| map.get(&target.key).cloned());
    match back {
        Some(back) if Fingerprint::of(back.expose_secret()) == *wanted => Ok(()),
        _ => Err(MirrorError::ReadbackMismatch),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::fake::FakeBackend;
    use crate::secrets::SecretMap;

    const SECTION: &str = r#"
        [backends.src]
        kind = "keychain"

        [backends.dst]
        kind = "keychain"

        [backends.dst2]
        kind = "keychain"

        [backends.op]
        kind = "onepassword"
        account = "my.1password.com"

        [mirrors.signing-key]
        source = { backend = "src", path = "deploy", key = "token" }
        targets = [
          { backend = "dst", path = "exec/github", key = "token" },
          { backend = "dst2", path = "exec/other", key = "token" },
        ]
    "#;

    fn section(text: &str) -> SecretsSection {
        toml::from_str(text).expect("section")
    }

    fn one() -> Mirror {
        section(SECTION).resolve_mirrors().expect("mirrors").remove(0)
    }

    fn key(name: &str) -> SecretKey {
        SecretKey::parse(name).expect("key")
    }

    fn path(raw: &str) -> SecretPath {
        SecretPath::parse(raw).expect("path")
    }

    struct World {
        source: FakeBackend,
        first: FakeBackend,
        second: FakeBackend,
    }

    impl World {
        fn new() -> Self {
            Self {
                source: FakeBackend::new(),
                first: FakeBackend::new(),
                second: FakeBackend::new(),
            }
        }

        fn resolve(&self) -> impl Fn(&str) -> Result<Backend, BackendError> + '_ {
            |name| match name {
                "src" => Ok(Backend::Fake(self.source.clone())),
                "dst" => Ok(Backend::Fake(self.first.clone())),
                _ => Ok(Backend::Fake(self.second.clone())),
            }
        }

        async fn seed(backend: &FakeBackend, at: &str, entries: &[(&str, &str)]) {
            let mut map = SecretMap::new();
            for (k, v) in entries {
                map.insert(key(k), SecretString::from(v.to_string()));
            }
            backend.set(&path(at), &map).await.expect("seed");
        }

        async fn value(backend: &FakeBackend, at: &str, name: &str) -> Option<String> {
            let map = backend.get(&path(at)).await.expect("get")?;
            map.get(&key(name)).map(|v| v.expose_secret().to_string())
        }

        async fn first_value(&self, name: &str) -> Option<String> {
            Self::value(&self.first, "exec/github", name).await
        }

        async fn second_value(&self, name: &str) -> Option<String> {
            Self::value(&self.second, "exec/other", name).await
        }
    }

    fn outcomes(run: TargetOutcomes) -> Vec<Result<MirrorOutcome, String>> {
        run.into_iter().map(|r| r.map_err(|e| e.to_string())).collect()
    }

    fn resolve_err(text: &str) -> MirrorConfigError {
        section(text).resolve_mirrors().expect_err("refused")
    }

    #[test]
    fn a_fingerprint_leads_with_the_length_of_the_value_without_one_trailing_newline() {
        let rendered = Fingerprint::of("abc\n").to_string();
        assert!(rendered.starts_with("3:"), "{rendered}");
        assert_eq!(Fingerprint::of("abc\n"), Fingerprint::of("abc"));
        assert_ne!(Fingerprint::of("abc\n\n"), Fingerprint::of("abc"));
        assert_ne!(Fingerprint::of("abc"), Fingerprint::of("abd"));
    }

    #[test]
    fn an_empty_value_is_never_equal_to_a_present_one() {
        assert!(Fingerprint::of("").is_empty());
        assert!(Fingerprint::of("\n").is_empty());
        assert!(!Fingerprint::of("x").is_empty());
        assert_ne!(Fingerprint::of(""), Fingerprint::of("x"));
    }

    #[test]
    fn a_fingerprint_debug_does_not_carry_the_digest() {
        let value = Fingerprint::of("hunter2");
        let digest = value.to_string();
        let rendered = format!("{value:?}");
        assert!(!rendered.contains(&digest[2..]), "{rendered}");
    }

    #[test]
    fn a_name_is_what_a_toml_bare_key_holds() {
        assert!(MirrorName::parse("signing-key_2").is_ok());
        assert_eq!(MirrorName::parse(""), Err(MirrorNameError::Empty));
        for bad in ["a b", "a.b", "a/b", "caf\u{e9}"] {
            assert!(
                matches!(MirrorName::parse(bad), Err(MirrorNameError::Characters(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_table_with_a_bad_name_does_not_parse() {
        let text = r#"
            [mirrors."a b"]
            source = { backend = "src", path = "a", key = "k" }
            targets = [{ backend = "dst", path = "b", key = "k" }]
        "#;

        assert!(toml::from_str::<SecretsSection>(text).is_err());
    }

    #[tokio::test]
    async fn a_missing_target_would_be_created_and_a_dry_run_writes_nothing() {
        let world = World::new();
        World::seed(&world.source, "deploy", &[("token", "s3cret")]).await;

        let run = mirror(&world.resolve(), &one(), false).await.expect("mirror");

        assert_eq!(
            outcomes(run),
            vec![Ok(MirrorOutcome::WouldCreate), Ok(MirrorOutcome::WouldCreate)]
        );
        assert_eq!(world.first_value("token").await, None);
        assert_eq!(world.second_value("token").await, None);
    }

    #[tokio::test]
    async fn the_source_is_read_once_however_many_targets_there_are() {
        let world = World::new();
        World::seed(&world.source, "deploy", &[("token", "s3cret")]).await;
        let before = world.source.get_count();

        mirror(&world.resolve(), &one(), true).await.expect("mirror");

        assert_eq!(world.source.get_count() - before, 1);
    }

    #[tokio::test]
    async fn applying_creates_every_target_and_records_where_it_came_from() {
        let world = World::new();
        World::seed(&world.source, "deploy", &[("token", "s3cret")]).await;

        let run = mirror(&world.resolve(), &one(), true).await.expect("mirror");

        assert_eq!(
            outcomes(run),
            vec![Ok(MirrorOutcome::Created), Ok(MirrorOutcome::Created)]
        );
        for value in [world.first_value("token").await, world.second_value("token").await] {
            assert_eq!(value.as_deref(), Some("s3cret"));
        }
        assert_eq!(world.first_value("source").await.as_deref(), Some("src:deploy#token"));
        assert_eq!(world.second_value("source").await.as_deref(), Some("src:deploy#token"));
    }

    #[tokio::test]
    async fn a_target_that_differs_is_updated_and_its_other_keys_survive() {
        let world = World::new();
        World::seed(&world.source, "deploy", &[("token", "new")]).await;
        World::seed(&world.first, "exec/github", &[("token", "old"), ("unrelated", "keep")]).await;
        World::seed(&world.second, "exec/other", &[("token", "new")]).await;

        let dry = mirror(&world.resolve(), &one(), false).await.expect("mirror");
        assert_eq!(
            outcomes(dry),
            vec![Ok(MirrorOutcome::WouldUpdate), Ok(MirrorOutcome::UpToDate)]
        );
        assert_eq!(world.first_value("token").await.as_deref(), Some("old"));

        let applied = mirror(&world.resolve(), &one(), true).await.expect("mirror");
        assert_eq!(
            outcomes(applied),
            vec![Ok(MirrorOutcome::Updated), Ok(MirrorOutcome::UpToDate)]
        );
        assert_eq!(world.first_value("token").await.as_deref(), Some("new"));
        assert_eq!(world.first_value("unrelated").await.as_deref(), Some("keep"));
    }

    /// A backend that adds a trailing newline on the way in holds the same
    /// value, so mirroring it again must not rewrite it.
    #[tokio::test]
    async fn a_target_that_differs_only_by_a_trailing_newline_is_up_to_date() {
        let world = World::new();
        World::seed(&world.source, "deploy", &[("token", "s3cret")]).await;
        World::seed(&world.first, "exec/github", &[("token", "s3cret\n")]).await;
        World::seed(&world.second, "exec/other", &[("token", "s3cret\n")]).await;

        let run = mirror(&world.resolve(), &one(), true).await.expect("mirror");

        assert_eq!(
            outcomes(run),
            vec![Ok(MirrorOutcome::UpToDate), Ok(MirrorOutcome::UpToDate)]
        );
        assert_eq!(world.first_value("source").await, None, "nothing was written");
    }

    #[tokio::test]
    async fn an_empty_source_is_refused_even_over_an_existing_target() {
        let world = World::new();
        World::seed(&world.source, "deploy", &[("token", "")]).await;
        World::seed(&world.first, "exec/github", &[("token", "old")]).await;

        let err = mirror(&world.resolve(), &one(), true).await.expect_err("refused");

        assert!(matches!(err, MirrorError::EmptySource), "{err}");
        assert_eq!(world.first_value("token").await.as_deref(), Some("old"));
    }

    #[tokio::test]
    async fn a_missing_source_fails_every_target_and_names_no_value() {
        let world = World::new();

        let err = mirror(&world.resolve(), &one(), false).await.expect_err("no source");

        assert!(matches!(err, MirrorError::Source(_)), "{err}");
    }

    /// One backend being down must not hide that the others are in sync or
    /// still drifted, nor stop them from being written.
    #[tokio::test]
    async fn one_failing_target_does_not_stop_the_others() {
        let world = World::new();
        World::seed(&world.source, "deploy", &[("token", "s3cret")]).await;
        world.first.set_set_failure(true);

        let run = mirror(&world.resolve(), &one(), true).await.expect("mirror");

        assert!(matches!(run[0], Err(MirrorError::TargetWrite(_))), "{:?}", run[0]);
        assert!(matches!(run[1], Ok(MirrorOutcome::Created)), "{:?}", run[1]);
        assert_eq!(world.second_value("token").await.as_deref(), Some("s3cret"));
    }

    /// A store that acknowledges a write and does not keep it must not be
    /// reported as mirrored.
    #[tokio::test]
    async fn a_target_that_reads_back_differently_fails_only_that_target() {
        let world = World::new();
        World::seed(&world.source, "deploy", &[("token", "s3cret")]).await;
        world.second.set_discard_writes(true);

        let run = mirror(&world.resolve(), &one(), true).await.expect("mirror");

        assert!(matches!(run[0], Ok(MirrorOutcome::Created)), "{:?}", run[0]);
        let Err(err) = &run[1] else { panic!("mismatch expected") };
        assert!(matches!(err, MirrorError::ReadbackMismatch), "{err}");
        assert_eq!(err.error_type(), "readback_mismatch");
    }

    #[test]
    fn mirrors_resolve_in_name_order() {
        let text = format!(
            r#"{SECTION}
            [mirrors.alpha]
            source = {{ backend = "src", path = "deploy", key = "token" }}
            targets = [{{ backend = "dst", path = "exec/alpha", key = "token" }}]
            "#
        );

        let names: Vec<String> = section(&text)
            .resolve_mirrors()
            .expect("mirrors")
            .iter()
            .map(|m| m.name().to_string())
            .collect();

        assert_eq!(names, ["alpha", "signing-key"]);
    }

    #[test]
    fn a_mirror_without_targets_is_refused_naming_the_table() {
        let text = r#"
            [backends.src]
            kind = "keychain"

            [mirrors.empty]
            source = { backend = "src", path = "a", key = "k" }
            targets = []
        "#;

        let err = resolve_err(text);

        assert!(matches!(err, MirrorConfigError::NoTargets { .. }), "{err}");
        assert!(err.to_string().contains("[secrets.mirrors.empty]"), "{err}");
    }

    #[test]
    fn a_singular_target_is_not_a_field() {
        let text = r#"
            [mirrors.m]
            source = { backend = "src", path = "a", key = "k" }
            target = { backend = "dst", path = "b", key = "k" }
        "#;

        assert!(toml::from_str::<SecretsSection>(text).is_err());
    }

    #[test]
    fn a_target_that_trg_cannot_write_is_refused_naming_the_table() {
        let text = format!(
            r#"{SECTION}
            [mirrors.readonly]
            source = {{ backend = "src", path = "a", key = "k" }}
            targets = [{{ backend = "op", ref = "op://Engineering/deploy-key/token" }}]
            "#
        );

        let err = resolve_err(&text);

        assert!(matches!(err, MirrorConfigError::TargetNotWritable { .. }), "{err}");
        assert!(err.to_string().contains("[secrets.mirrors.readonly]"), "{err}");
    }

    #[test]
    fn a_mirror_onto_itself_is_refused() {
        let text = r#"
            [backends.src]
            kind = "keychain"

            [backends.dst]
            kind = "keychain"

            [mirrors.loop]
            source = { backend = "src", path = "a", key = "k" }
            targets = [
              { backend = "dst", path = "b", key = "k" },
              { backend = "src", path = "a", key = "k" },
            ]
        "#;

        let err = resolve_err(text);

        assert!(matches!(err, MirrorConfigError::SameAddress { .. }), "{err}");
        assert!(err.to_string().contains("[secrets.mirrors.loop]"), "{err}");
    }

    #[test]
    fn a_backend_that_is_not_declared_is_refused_naming_the_table_and_side() {
        let text = r#"
            [backends.src]
            kind = "keychain"

            [mirrors.m]
            source = { backend = "src", path = "a", key = "k" }
            targets = [{ backend = "ghost", path = "b", key = "k" }]
        "#;

        let rendered = resolve_err(text).to_string();

        assert!(rendered.contains("[secrets.mirrors.m]"), "{rendered}");
        assert!(rendered.contains("targets[0]"), "{rendered}");
        assert!(rendered.contains("ghost"), "{rendered}");
    }

    #[test]
    fn a_target_listed_twice_in_one_mirror_is_refused() {
        let text = r#"
            [backends.src]
            kind = "keychain"

            [backends.dst]
            kind = "keychain"

            [mirrors.m]
            source = { backend = "src", path = "a", key = "k" }
            targets = [
              { backend = "dst", path = "b", key = "k" },
              { backend = "dst", path = "b", key = "k" },
            ]
        "#;

        let err = resolve_err(text);

        assert!(matches!(err, MirrorConfigError::RepeatedTarget { .. }), "{err}");
    }

    #[test]
    fn two_mirrors_may_not_write_the_same_target_and_the_error_names_both() {
        let text = format!(
            r#"{SECTION}
            [mirrors.zeta]
            source = {{ backend = "src", path = "other", key = "token" }}
            targets = [{{ backend = "dst", path = "exec/github", key = "token" }}]
            "#
        );

        let err = resolve_err(&text);

        assert!(matches!(err, MirrorConfigError::DuplicateTarget { .. }), "{err}");
        let rendered = err.to_string();
        assert!(rendered.contains("[secrets.mirrors.zeta]"), "{rendered}");
        assert!(rendered.contains("[secrets.mirrors.signing-key]"), "{rendered}");
    }

    #[test]
    fn two_mirrors_with_different_sources_may_not_share_a_target_path() {
        let text = format!(
            r#"{SECTION}
            [mirrors.zeta]
            source = {{ backend = "src", path = "other", key = "token" }}
            targets = [{{ backend = "dst", path = "exec/github", key = "other" }}]
            "#
        );

        let err = resolve_err(&text);

        assert!(matches!(err, MirrorConfigError::ConflictingSource { .. }), "{err}");
        let rendered = err.to_string();
        assert!(rendered.contains("[secrets.mirrors.zeta]"), "{rendered}");
        assert!(rendered.contains("[secrets.mirrors.signing-key]"), "{rendered}");
        assert!(rendered.contains("exec/github"), "{rendered}");
    }

    #[test]
    fn two_mirrors_with_the_same_source_may_share_a_path_under_different_keys() {
        let text = format!(
            r#"{SECTION}
            [mirrors.zeta]
            source = {{ backend = "src", path = "deploy", key = "token" }}
            targets = [{{ backend = "dst", path = "exec/github", key = "other" }}]
            "#
        );

        assert!(section(&text).resolve_mirrors().is_ok());
    }

    #[test]
    fn the_source_key_is_reserved_on_the_target() {
        let text = r#"
            [backends.src]
            kind = "keychain"

            [mirrors.m]
            source = { backend = "src", path = "a", key = "k" }
            targets = [{ backend = "src", path = "b", key = "source" }]
        "#;

        let err = resolve_err(text);

        assert!(matches!(err, MirrorConfigError::ReservedKey { .. }), "{err}");
    }

    #[test]
    fn a_one_password_source_is_recorded_as_the_reference_it_was_written_with() {
        let text = r#"
            [backends.op]
            kind = "onepassword"
            account = "my.1password.com"

            [backends.dst]
            kind = "keychain"

            [mirrors.deploy]
            source = { backend = "op", ref = "op://Engineering/deploy-key/private key" }
            targets = [{ backend = "dst", path = "exec/github", key = "token" }]
        "#;

        let mirrors = section(text).resolve_mirrors().expect("mirrors");

        assert_eq!(mirrors[0].source_reference(), "op://Engineering/deploy-key/private key");
    }

    #[test]
    fn a_source_reference_keeps_its_query() {
        let text = r#"
            [backends.op]
            kind = "onepassword"
            account = "my.1password.com"

            [backends.dst]
            kind = "keychain"

            [mirrors.deploy]
            source = { backend = "op", ref = "op://Engineering/deploy-key/private key?ssh-format=openssh" }
            targets = [{ backend = "dst", path = "exec/deploy", key = "ssh_private_key" }]
        "#;

        let mirrors = section(text).resolve_mirrors().expect("mirrors");

        assert_eq!(
            mirrors[0].source_reference(),
            "op://Engineering/deploy-key/private key?ssh-format=openssh"
        );
        assert_eq!(mirrors[0].targets()[0].describe(), "exec/deploy#ssh_private_key (dst)");
    }

    #[test]
    fn an_unknown_field_in_a_mirror_is_rejected() {
        let text = r#"
            [mirrors.m]
            source = { backend = "src", path = "a", key = "k" }
            targets = [{ backend = "dst", path = "b", key = "k" }]
            extra = true
        "#;

        assert!(toml::from_str::<SecretsSection>(text).is_err());
    }
}
