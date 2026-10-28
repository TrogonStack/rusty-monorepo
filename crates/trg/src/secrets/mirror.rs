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

use super::vars::{self, VarFetchError};
use super::{Backend, BackendError, BackendKind, SecretAddress, SecretKey, SecretPath, SecretsError, SecretsSection};
use crate::config::{RawSecretVar, SecretVar, SecretVarError, VarSite};

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

/// One `[[secrets.mirrors]]` entry as the file spelled it.
///
/// Both sides are the inline table a var is written as, so a value is
/// addressed here exactly as it is anywhere else in the config.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MirrorConfig {
    pub source: RawSecretVar,
    pub target: RawSecretVar,
}

/// A mirror whose two sides have been checked against the declared backends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mirror {
    source: SecretVar,
    target: SecretVar,
    target_path: SecretPath,
    target_key: SecretKey,
}

#[derive(Debug, thiserror::Error)]
pub enum MirrorConfigError {
    #[error("{0}")]
    Var(#[from] SecretVarError),

    #[error(
        "`[[secrets.mirrors]]` entry {index}: target backend `{backend}` is of kind `{kind}`, which `trg` only reads from; mirror into a `keychain` or `openbao` backend"
    )]
    TargetNotWritable {
        index: usize,
        backend: String,
        kind: BackendKind,
    },

    #[error("`[[secrets.mirrors]]` entry {index}: source and target are the same address, {address}")]
    SameAddress { index: usize, address: String },

    #[error(
        "`[[secrets.mirrors]]` entry {index}: target key `{SOURCE_KEY}` is reserved for the reference to where the value came from"
    )]
    ReservedKey { index: usize },

    #[error("`[[secrets.mirrors]]` entry {index}: target {target} is already the target of entry {first}")]
    DuplicateTarget { index: usize, first: usize, target: String },
}

impl Mirror {
    pub fn source(&self) -> &SecretVar {
        &self.source
    }

    pub fn target(&self) -> &SecretVar {
        &self.target
    }

    /// Where the value came from, as written into the target's `source` key.
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

/// Resolve every declared mirror, numbering entries from 1 in file order.
pub fn resolve_all(secrets: &SecretsSection) -> Result<Vec<Mirror>, MirrorConfigError> {
    let mut out = Vec::with_capacity(secrets.mirrors.len());
    let mut targets: HashMap<SecretVar, usize> = HashMap::new();

    for (offset, config) in secrets.mirrors.iter().enumerate() {
        let index = offset + 1;
        let mirror = resolve_one(index, config, secrets)?;
        if let Some(&first) = targets.get(&mirror.target) {
            return Err(MirrorConfigError::DuplicateTarget {
                index,
                first,
                target: mirror.target.to_string(),
            });
        }
        targets.insert(mirror.target.clone(), index);
        out.push(mirror);
    }

    Ok(out)
}

fn resolve_one(index: usize, config: &MirrorConfig, secrets: &SecretsSection) -> Result<Mirror, MirrorConfigError> {
    let source = SecretVar::resolve(&config.source, secrets, VarSite::mirror(index, "source"))?;
    let target = SecretVar::resolve(&config.target, secrets, VarSite::mirror(index, "target"))?;

    let kind = target.address().backend_kind();
    if !kind.is_writable() {
        return Err(MirrorConfigError::TargetNotWritable {
            index,
            backend: target.backend().to_string(),
            kind,
        });
    }
    if source == target {
        return Err(MirrorConfigError::SameAddress {
            index,
            address: source.to_string(),
        });
    }
    let Some((path, key)) = target.address().path_key() else {
        unreachable!("a writable kind is addressed by path and key")
    };
    if key.as_str() == SOURCE_KEY {
        return Err(MirrorConfigError::ReservedKey { index });
    }
    let (target_path, target_key) = (path.clone(), key.clone());

    Ok(Mirror {
        source,
        target,
        target_path,
        target_key,
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

/// Bring one target in line with its source, or report how it would.
///
/// A dry run reads both sides and writes nothing. An applied run writes only
/// when the fingerprints differ, then reads the target back and fails if what
/// it holds is not what was written, since a write the backend acknowledged
/// but did not keep must not be reported as mirrored.
pub async fn mirror<F>(resolve: &F, mirror: &Mirror, apply: bool) -> Result<MirrorOutcome, MirrorError>
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

    let target = resolve(mirror.target.backend()).map_err(|cause| MirrorError::TargetBackend {
        name: mirror.target.backend().to_string(),
        cause: Box::new(cause),
    })?;
    let existing = target.get(&mirror.target_path).await.map_err(MirrorError::TargetRead)?;
    let held = existing
        .as_ref()
        .and_then(|map| map.get(&mirror.target_key))
        .map(|held| Fingerprint::of(held.expose_secret()));

    let outcome = match &held {
        None => MirrorOutcome::WouldCreate,
        Some(held) if *held == wanted => return Ok(MirrorOutcome::UpToDate),
        Some(_) => MirrorOutcome::WouldUpdate,
    };
    if !apply {
        return Ok(outcome);
    }

    let mut map = existing.unwrap_or_default();
    map.insert(mirror.target_key.clone(), value.clone());
    let Ok(source_key) = SecretKey::parse(SOURCE_KEY) else {
        unreachable!("`{SOURCE_KEY}` is a valid key")
    };
    map.insert(source_key, SecretString::from(mirror.source_reference()));
    target
        .set(&mirror.target_path, &map)
        .await
        .map_err(MirrorError::TargetWrite)?;

    verify_readback(&target, mirror, &wanted).await?;

    Ok(match outcome {
        MirrorOutcome::WouldCreate => MirrorOutcome::Created,
        _ => MirrorOutcome::Updated,
    })
}

async fn verify_readback(target: &Backend, mirror: &Mirror, wanted: &Fingerprint) -> Result<(), MirrorError> {
    let back = target
        .get(&mirror.target_path)
        .await
        .map_err(MirrorError::TargetRead)?
        .and_then(|map| map.get(&mirror.target_key).cloned());
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

        [backends.op]
        kind = "onepassword"
        account = "my.1password.com"

        [[mirrors]]
        source = { backend = "src", path = "deploy", key = "token" }
        target = { backend = "dst", path = "exec/github", key = "token" }
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
        target: FakeBackend,
    }

    impl World {
        fn new() -> Self {
            Self {
                source: FakeBackend::new(),
                target: FakeBackend::new(),
            }
        }

        fn resolve(&self) -> impl Fn(&str) -> Result<Backend, BackendError> + '_ {
            |name| match name {
                "src" => Ok(Backend::Fake(self.source.clone())),
                _ => Ok(Backend::Fake(self.target.clone())),
            }
        }

        async fn seed(backend: &FakeBackend, at: &str, entries: &[(&str, &str)]) {
            let mut map = SecretMap::new();
            for (k, v) in entries {
                map.insert(key(k), SecretString::from(v.to_string()));
            }
            backend.set(&path(at), &map).await.expect("seed");
        }

        async fn target_value(&self, name: &str) -> Option<String> {
            let map = self.target.get(&path("exec/github")).await.expect("get")?;
            map.get(&key(name)).map(|v| v.expose_secret().to_string())
        }
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

    #[tokio::test]
    async fn a_missing_target_would_be_created_and_a_dry_run_writes_nothing() {
        let world = World::new();
        World::seed(&world.source, "deploy", &[("token", "s3cret")]).await;

        let outcome = mirror(&world.resolve(), &one(), false).await.expect("mirror");

        assert_eq!(outcome, MirrorOutcome::WouldCreate);
        assert_eq!(world.target_value("token").await, None);
    }

    #[tokio::test]
    async fn applying_creates_the_target_and_records_where_it_came_from() {
        let world = World::new();
        World::seed(&world.source, "deploy", &[("token", "s3cret")]).await;

        let outcome = mirror(&world.resolve(), &one(), true).await.expect("mirror");

        assert_eq!(outcome, MirrorOutcome::Created);
        assert_eq!(world.target_value("token").await.as_deref(), Some("s3cret"));
        assert_eq!(world.target_value("source").await.as_deref(), Some("src:deploy#token"));
    }

    #[tokio::test]
    async fn a_target_that_differs_is_updated_and_its_other_keys_survive() {
        let world = World::new();
        World::seed(&world.source, "deploy", &[("token", "new")]).await;
        World::seed(&world.target, "exec/github", &[("token", "old"), ("unrelated", "keep")]).await;

        let dry = mirror(&world.resolve(), &one(), false).await.expect("mirror");
        assert_eq!(dry, MirrorOutcome::WouldUpdate);
        assert_eq!(world.target_value("token").await.as_deref(), Some("old"));

        let applied = mirror(&world.resolve(), &one(), true).await.expect("mirror");
        assert_eq!(applied, MirrorOutcome::Updated);
        assert_eq!(world.target_value("token").await.as_deref(), Some("new"));
        assert_eq!(world.target_value("unrelated").await.as_deref(), Some("keep"));
    }

    /// A backend that adds a trailing newline on the way in holds the same
    /// value, so mirroring it again must not rewrite it.
    #[tokio::test]
    async fn a_target_that_differs_only_by_a_trailing_newline_is_up_to_date() {
        let world = World::new();
        World::seed(&world.source, "deploy", &[("token", "s3cret")]).await;
        World::seed(&world.target, "exec/github", &[("token", "s3cret\n")]).await;

        let outcome = mirror(&world.resolve(), &one(), true).await.expect("mirror");

        assert_eq!(outcome, MirrorOutcome::UpToDate);
        assert_eq!(world.target_value("source").await, None, "nothing was written");
    }

    #[tokio::test]
    async fn an_empty_source_is_refused_even_over_an_existing_target() {
        let world = World::new();
        World::seed(&world.source, "deploy", &[("token", "")]).await;
        World::seed(&world.target, "exec/github", &[("token", "old")]).await;

        let err = mirror(&world.resolve(), &one(), true).await.expect_err("refused");

        assert!(matches!(err, MirrorError::EmptySource), "{err}");
        assert_eq!(world.target_value("token").await.as_deref(), Some("old"));
    }

    #[tokio::test]
    async fn a_missing_source_fails_and_names_no_value() {
        let world = World::new();

        let err = mirror(&world.resolve(), &one(), false).await.expect_err("no source");

        assert!(matches!(err, MirrorError::Source(_)), "{err}");
    }

    /// A store that acknowledges a write and does not keep it must not be
    /// reported as mirrored.
    #[tokio::test]
    async fn a_target_that_reads_back_differently_fails_the_mirror() {
        let world = World::new();
        World::seed(&world.source, "deploy", &[("token", "s3cret")]).await;
        let resolve = |name: &str| match name {
            "src" => Ok(Backend::Fake(world.source.clone())),
            _ => Ok(Backend::Fake(world.target.clone())),
        };
        world.target.set_discard_writes(true);

        let err = mirror(&resolve, &one(), true).await.expect_err("mismatch");

        assert!(matches!(err, MirrorError::ReadbackMismatch), "{err}");
        assert_eq!(err.error_type(), "readback_mismatch");
    }

    #[tokio::test]
    async fn a_failed_write_is_reported_as_a_write_failure() {
        let world = World::new();
        World::seed(&world.source, "deploy", &[("token", "s3cret")]).await;
        world.target.set_set_failure(true);

        let err = mirror(&world.resolve(), &one(), true).await.expect_err("write fails");

        assert!(matches!(err, MirrorError::TargetWrite(_)), "{err}");
    }

    #[test]
    fn a_target_that_trg_cannot_write_is_refused_naming_the_entry() {
        let text = format!(
            r#"{SECTION}
            [[mirrors]]
            source = {{ backend = "src", path = "a", key = "k" }}
            target = {{ backend = "op", ref = "op://Engineering/deploy-key/token" }}
            "#
        );

        let err = section(&text).resolve_mirrors().expect_err("read-only target");

        assert!(
            matches!(err, MirrorConfigError::TargetNotWritable { index: 2, .. }),
            "{err}"
        );
        assert!(err.to_string().contains("entry 2"), "{err}");
    }

    #[test]
    fn a_mirror_onto_itself_is_refused() {
        let text = r#"
            [backends.src]
            kind = "keychain"

            [[mirrors]]
            source = { backend = "src", path = "a", key = "k" }
            target = { backend = "src", path = "a", key = "k" }
        "#;

        let err = section(text).resolve_mirrors().expect_err("same address");

        assert!(matches!(err, MirrorConfigError::SameAddress { index: 1, .. }), "{err}");
    }

    #[test]
    fn a_backend_that_is_not_declared_is_refused_naming_the_entry_and_side() {
        let text = r#"
            [backends.src]
            kind = "keychain"

            [[mirrors]]
            source = { backend = "src", path = "a", key = "k" }
            target = { backend = "ghost", path = "b", key = "k" }
        "#;

        let err = section(text).resolve_mirrors().expect_err("undeclared");

        let rendered = err.to_string();
        assert!(rendered.contains("entry 1 target"), "{rendered}");
        assert!(rendered.contains("ghost"), "{rendered}");
    }

    #[test]
    fn two_mirrors_may_not_write_the_same_target() {
        let text = format!(
            r#"{SECTION}
            [[mirrors]]
            source = {{ backend = "src", path = "other", key = "token" }}
            target = {{ backend = "dst", path = "exec/github", key = "token" }}
            "#
        );

        let err = section(&text).resolve_mirrors().expect_err("duplicate");

        assert!(
            matches!(err, MirrorConfigError::DuplicateTarget { index: 2, first: 1, .. }),
            "{err}"
        );
    }

    #[test]
    fn the_source_key_is_reserved_on_the_target() {
        let text = r#"
            [backends.src]
            kind = "keychain"

            [[mirrors]]
            source = { backend = "src", path = "a", key = "k" }
            target = { backend = "src", path = "b", key = "source" }
        "#;

        let err = section(text).resolve_mirrors().expect_err("reserved");

        assert!(matches!(err, MirrorConfigError::ReservedKey { index: 1 }), "{err}");
    }

    #[test]
    fn a_one_password_source_is_recorded_as_the_reference_it_was_written_with() {
        let text = r#"
            [backends.op]
            kind = "onepassword"
            account = "my.1password.com"

            [backends.dst]
            kind = "openbao"
            addr = "http://127.0.0.1:8200"
            mount = "secret"
            path_prefix = "trg"
            owner = "me"
            token = "t"

            [[mirrors]]
            source = { backend = "op", ref = "op://Engineering/deploy-key/private key" }
            target = { backend = "dst", path = "exec/github", key = "token" }
        "#;

        let mirrors = section(text).resolve_mirrors().expect("mirrors");

        assert_eq!(mirrors[0].source_reference(), "op://Engineering/deploy-key/private key");
    }

    #[test]
    fn an_unknown_field_in_an_entry_is_rejected() {
        let text = r#"
            [[mirrors]]
            source = { backend = "src", path = "a", key = "k" }
            target = { backend = "dst", path = "b", key = "k" }
            extra = true
        "#;

        assert!(toml::from_str::<SecretsSection>(text).is_err());
    }
}
