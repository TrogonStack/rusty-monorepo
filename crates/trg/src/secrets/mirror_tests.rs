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
