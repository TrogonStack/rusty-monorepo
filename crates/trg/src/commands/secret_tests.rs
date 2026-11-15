use serde_json::json;

use super::*;
use crate::secrets::fake::FakeBackend;
use crate::secrets::telemetry::testing::{assert_never_recorded, current_thread};
use crate::secrets::KeychainReference;
use clap::{CommandFactory, Parser};
use opentelemetry::trace::Status;

/// A parser holding just the secret subcommands, so the flag grammar can
/// be exercised without going through the whole binary's argv.
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: SecretCommands,
}

fn parse(argv: &[&str]) -> Result<SecretCommands, clap::Error> {
    Cli::try_parse_from(std::iter::once("trg").chain(argv.iter().copied())).map(|c| c.command)
}

fn put_args(key: &str) -> PutArgs {
    PutArgs {
        common: CommonArgs {
            backend: "fake".to_string(),
            output_format: OutputFormat::Text,
        },
        path: "mcp/demo".to_string(),
        key: key.to_string(),
    }
}

fn path() -> SecretPath {
    SecretPath::parse("mcp/demo").expect("path")
}

fn key(name: &str) -> SecretKey {
    SecretKey::parse(name).expect("key")
}

fn keychain_var(name: &str) -> SecretVar {
    SecretVar::new(
        "fake".to_string(),
        SecretAddress::Keychain(KeychainReference::new(path(), key(name))),
    )
}

async fn write(backend: &Backend, name: &str, value: &str) -> bool {
    write_key(
        backend,
        &path(),
        key(name),
        SecretString::from(value.to_string()),
        &put_args(name),
    )
    .await
    .expect("write")
}

#[test]
fn the_flag_grammar_is_well_formed() {
    Cli::command().debug_assert();
}

fn get_args(argv: &[&str]) -> Result<GetArgs, clap::Error> {
    let mut full = vec![
        "get",
        "--backend",
        "homelab",
        "--path",
        "exec/github",
        "--key",
        "GH_TOKEN",
    ];
    full.extend_from_slice(argv);
    match parse(&full)? {
        SecretCommands::Get(args) => Ok(args),
        _ => panic!("expected get"),
    }
}

#[test]
fn get_takes_no_fallback_unless_asked_to() {
    assert!(get_args(&[]).unwrap().fallbacks.is_empty());
}

#[test]
fn get_takes_fallbacks_in_the_order_given() {
    let args = get_args(&[
        "--fallback",
        r#"{ backend = "op", ref = "op://Ops/github/GH_TOKEN" }"#,
        "--fallback",
        r#"{ backend = "other", path = "exec/github", key = "GH_TOKEN" }"#,
    ])
    .unwrap();

    assert_eq!(args.fallbacks.len(), 2);
    assert_eq!(args.fallbacks[0].backend, "op");
    assert_eq!(args.fallbacks[1].backend, "other");
}

#[test]
fn a_fallback_that_is_not_an_address_table_is_refused_at_parse_time() {
    assert!(get_args(&["--fallback", "not a table"]).is_err());
    assert!(get_args(&["--fallback", r#"{ backend = "op", typo = true }"#]).is_err());
}

#[test]
fn get_reads_a_secret_by_name_alone() {
    let SecretCommands::Get(args) = parse(&["get", "--name", "git-signing-key"]).unwrap() else {
        panic!("expected get");
    };
    assert_eq!(args.name.as_ref().map(SecretName::as_str), Some("git-signing-key"));
    assert!(args.backend.is_none());
}

#[test]
fn a_name_cannot_be_combined_with_any_address_flag() {
    for extra in [
        &["--backend", "b"][..],
        &["--path", "p", "--key", "k"][..],
        &["--ref", "op://v/i/f"][..],
        &["--fallback", r#"{ backend = "op", ref = "op://v/i/f" }"#][..],
    ] {
        let mut argv = vec!["get", "--name", "n"];
        argv.extend_from_slice(extra);
        let err = parse(&argv).err().expect("refused");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict, "{extra:?}: {err}");
    }
}

#[test]
fn a_name_that_is_not_a_secret_name_is_refused_by_clap() {
    let err = parse(&["get", "--name", "bad name!"]).err().expect("refused");
    assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation, "{err}");
    assert!(err.to_string().contains("secret name"), "{err}");
}

#[test]
fn get_needs_a_name_or_an_address() {
    assert!(parse(&["get"]).is_err());
    assert!(parse(&["get", "--backend", "b"]).is_err());
}

#[test]
fn put_has_no_name_flag() {
    assert!(parse(&["put", "--backend", "b", "--path", "p", "--key", "k", "--name", "n"]).is_err());
}

#[test]
fn put_has_no_fallback_flag() {
    assert!(parse(&["put", "--backend", "b", "--path", "p", "--key", "k", "--fallback", "{}"]).is_err());
}

#[tokio::test]
async fn a_first_write_reports_that_nothing_was_there() {
    let backend = Backend::Fake(FakeBackend::new());
    assert!(!write(&backend, "token", "one").await);
}

#[tokio::test]
async fn writing_over_a_key_reports_the_replacement() {
    let backend = Backend::Fake(FakeBackend::new());
    write(&backend, "token", "one").await;
    assert!(write(&backend, "token", "two").await);

    let got = read(&backend, &keychain_var("token")).await.expect("read");
    assert_eq!(got.expose_secret(), "two");
}

#[tokio::test]
async fn a_sibling_key_survives_a_write_next_to_it() {
    let backend = Backend::Fake(FakeBackend::new());
    write(&backend, "token", "one").await;
    write(&backend, "refresh", "two").await;

    let token = read(&backend, &keychain_var("token")).await.expect("read");
    assert_eq!(token.expose_secret(), "one");
}

#[tokio::test]
async fn an_unwritten_path_is_named_rather_than_read_as_empty() {
    let backend = Backend::Fake(FakeBackend::new());
    let err = read(&backend, &keychain_var("token")).await.expect_err("no entry");
    assert!(err.contains("no entry at `mcp/demo`"), "{err}");
}

#[tokio::test]
async fn a_missing_key_names_its_siblings_and_no_value() {
    let backend = Backend::Fake(FakeBackend::new());
    write(&backend, "token", "super-secret").await;
    write(&backend, "refresh", "also-secret").await;

    let err = read(&backend, &keychain_var("nope")).await.expect_err("no key");
    assert!(err.contains("refresh"), "{err}");
    assert!(err.contains("token"), "{err}");
    assert!(!err.contains("super-secret"), "{err}");
    assert!(!err.contains("also-secret"), "{err}");
}

#[tokio::test]
async fn a_query_reference_tells_a_missing_field_apart_from_a_missing_item() {
    let backend = Backend::Fake(FakeBackend::new());
    let mut map = SecretMap::new();
    map.insert(key("TOKEN"), SecretString::from("super-secret".to_string()));
    backend
        .set(&SecretPath::parse("Ops/deploy-keys").expect("path"), &map)
        .await
        .expect("seed");
    let var = |raw: &str| {
        SecretVar::new(
            "fake".to_string(),
            SecretAddress::OnePassword(Box::new(OnePasswordReference::parse(raw).expect("reference"))),
        )
    };

    let field = read(&backend, &var("op://Ops/deploy-keys/nope?ssh-format=openssh"))
        .await
        .expect_err("no field");
    assert!(field.contains("no field at"), "{field}");
    assert!(!field.contains("no item"), "{field}");

    let item = read(&backend, &var("op://Ops/absent/TOKEN?ssh-format=openssh"))
        .await
        .expect_err("no item");
    assert!(item.contains("no item `"), "{item}");
}

/// A 1Password item answers with every field at once, so a reference that
/// resolves to nothing has the rest of the item to list as the hint.
#[tokio::test]
async fn a_reference_reads_the_field_it_names_and_names_the_others_when_it_misses() {
    let backend = Backend::Fake(FakeBackend::new());
    let mut map = SecretMap::new();
    map.insert(key("TOKEN"), SecretString::from("super-secret".to_string()));
    map.insert(key("Prod/TOKEN"), SecretString::from("also-secret".to_string()));
    backend
        .set(&SecretPath::parse("Ops/deploy-keys").expect("path"), &map)
        .await
        .expect("seed");

    let var = |raw: &str| {
        SecretVar::new(
            "fake".to_string(),
            SecretAddress::OnePassword(Box::new(OnePasswordReference::parse(raw).expect("reference"))),
        )
    };

    let plain = read(&backend, &var("op://Ops/deploy-keys/TOKEN")).await.expect("read");
    assert_eq!(plain.expose_secret(), "super-secret");

    let sectioned = read(&backend, &var("op://Ops/deploy-keys/Prod/TOKEN"))
        .await
        .expect("read");
    assert_eq!(sectioned.expose_secret(), "also-secret");

    let err = read(&backend, &var("op://Ops/deploy-keys/ABSENT"))
        .await
        .expect_err("no such field");
    assert!(err.contains("Prod/TOKEN"), "{err}");
    assert!(!err.contains("super-secret"), "{err}");
}

/// The document names the address the way it was given, so a script that
/// reads one back is not left guessing which of two forms it holds.
#[test]
fn the_json_document_carries_only_the_form_that_was_used() {
    let value = SecretString::from("super-secret".to_string());

    let by_path = document(&put_args("token").raw(), &value);
    assert_eq!(by_path["path"], json!("mcp/demo"));
    assert_eq!(by_path["value"], json!("super-secret"));
    assert!(by_path.get("ref").is_none());

    let by_reference = document(
        &RawSecretVar {
            backend: "op".to_string(),
            path: None,
            key: None,
            reference: Some("op://Ops/deploy-keys/TOKEN".to_string()),
        },
        &value,
    );
    assert_eq!(by_reference["ref"], json!("op://Ops/deploy-keys/TOKEN"));
    assert!(by_reference.get("path").is_none());
    assert!(by_reference.get("key").is_none());
}

#[test]
fn a_declaration_can_be_pasted_into_a_vars_table() {
    let line = put_args("token").raw().declaration();
    assert_eq!(line, r#"{ backend = "fake", path = "mcp/demo", key = "token" }"#);
}

/// The two vocabularies are not mixable, and clap is what says so, before
/// anything has looked up what kind of backend was named.
#[test]
fn a_reference_and_a_path_cannot_be_given_together() {
    assert!(parse(&[
        "get",
        "--backend",
        "op",
        "--ref",
        "op://Ops/deploy-keys/TOKEN",
        "--path",
        "Ops/deploy-keys",
        "--key",
        "TOKEN",
    ])
    .is_err());
}

#[test]
fn half_of_a_path_and_key_address_is_refused() {
    assert!(parse(&["get", "--backend", "local", "--path", "mcp/demo"]).is_err());
    assert!(parse(&["get", "--backend", "local", "--key", "token"]).is_err());
}

#[test]
fn an_address_is_required() {
    assert!(parse(&["get", "--backend", "local"]).is_err());
}

/// `put` writes, and the only backend addressed by reference is one `trg`
/// cannot write to, so the flag is not on this subcommand at all.
#[test]
fn put_takes_no_reference() {
    assert!(parse(&["put", "--backend", "op", "--ref", "op://Ops/deploy-keys/TOKEN"]).is_err());
}

#[test]
fn both_forms_parse_on_their_own() {
    assert!(parse(&["get", "--backend", "local", "--path", "mcp/demo", "--key", "token"]).is_ok());
    assert!(parse(&["get", "--backend", "op", "--ref", "op://Ops/deploy-keys/TOKEN"]).is_ok());
}

#[test]
fn a_mirror_is_a_dry_run_unless_it_is_told_to_apply() {
    let Ok(SecretCommands::Mirror(dry)) = parse(&["mirror"]) else {
        panic!("`mirror` should parse")
    };
    assert!(!dry.apply(), "nothing is written by default");

    let Ok(SecretCommands::Mirror(applied)) = parse(&["mirror", "apply"]) else {
        panic!("`mirror apply` should parse")
    };
    assert!(applied.apply(), "apply is what writes");
}

#[test]
fn the_apply_flag_is_not_accepted() {
    assert!(parse(&["mirror", "--apply"]).is_err());
}

#[test]
fn a_mirror_can_be_picked_by_name() {
    let Ok(SecretCommands::Mirror(dry)) = parse(&["mirror", "--name", "a", "--name", "b"]) else {
        panic!("names should parse")
    };
    assert!(!dry.apply());
    assert_eq!(dry.selection().names, ["a", "b"]);

    let Ok(SecretCommands::Mirror(applied)) = parse(&["mirror", "apply", "--name", "a", "--name", "b"]) else {
        panic!("names should parse after apply")
    };
    assert!(applied.apply());
    assert_eq!(applied.selection().names, ["a", "b"]);
}

/// A name is a flag value, so a mirror declared as `apply` is picked for a
/// preview rather than read as the subcommand that writes.
#[test]
fn a_mirror_named_like_a_subcommand_is_still_only_previewed() {
    let Ok(SecretCommands::Mirror(dry)) = parse(&["mirror", "--name", "apply"]) else {
        panic!("`--name apply` should parse")
    };
    assert!(!dry.apply());
    assert_eq!(dry.selection().names, ["apply"]);
}

#[test]
fn a_name_is_not_taken_positionally() {
    assert!(parse(&["mirror", "a"]).is_err());
}

fn declared() -> Vec<Mirror> {
    let section: crate::secrets::SecretsSection = toml::from_str(
        r#"
            [backends.src]
            kind = "keychain"

            [backends.dst]
            kind = "keychain"

            [mirrors.zeta]
            source = { backend = "src", path = "a", key = "k" }
            targets = [{ backend = "dst", path = "z", key = "k" }]

            [mirrors.alpha]
            source = { backend = "src", path = "a", key = "k" }
            targets = [{ backend = "dst", path = "al", key = "k" }]

            [mirrors.mid]
            source = { backend = "src", path = "a", key = "k" }
            targets = [{ backend = "dst", path = "m", key = "k" }]
            "#,
    )
    .expect("section");
    section.resolve_mirrors().expect("mirrors")
}

fn names(mirrors: &[Mirror]) -> Vec<String> {
    mirrors.iter().map(|m| m.name().to_string()).collect()
}

#[test]
fn no_names_selects_every_mirror_in_sorted_order() {
    let picked = select_mirrors(declared(), &[]).unwrap();
    assert_eq!(names(&picked), ["alpha", "mid", "zeta"]);
}

#[test]
fn a_subset_keeps_sorted_order_whatever_order_it_was_named_in() {
    let picked = select_mirrors(declared(), &["zeta".to_string(), "alpha".to_string()]).unwrap();
    assert_eq!(names(&picked), ["alpha", "zeta"]);
}

#[test]
fn a_name_given_twice_runs_its_mirror_once() {
    let picked = select_mirrors(declared(), &["mid".to_string(), "mid".to_string()]).unwrap();
    assert_eq!(names(&picked), ["mid"]);
}

#[test]
fn an_unknown_name_is_an_error_that_lists_the_declared_names() {
    let err = select_mirrors(declared(), &["alpha".to_string(), "ghost".to_string()]).unwrap_err();
    assert!(err.contains("`ghost`"), "{err}");
    assert!(err.contains("`alpha`, `mid`, `zeta`"), "{err}");
}

#[test]
fn the_summary_counts_what_each_target_did_and_points_at_apply_only_when_it_would_help() {
    let broken = MirrorError::EmptySource;
    let results = vec![
        vec![Ok(MirrorOutcome::UpToDate), Ok(MirrorOutcome::WouldCreate)],
        vec![Ok(MirrorOutcome::WouldUpdate), Err(&broken)],
    ];
    assert_eq!(
        mirror_summary(&results, false),
        "4 targets: 1 up to date, 1 would create, 1 would update, 1 failed; run `trg secret mirror apply` to write"
    );

    let applied = vec![vec![Ok(MirrorOutcome::Created), Ok(MirrorOutcome::Updated)]];
    assert_eq!(
        mirror_summary(&applied, true),
        "2 targets: 0 up to date, 1 created, 1 updated, 0 failed"
    );

    let settled = vec![vec![Ok(MirrorOutcome::UpToDate)]];
    assert_eq!(
        mirror_summary(&settled, false),
        "1 target: 1 up to date, 0 would create, 0 would update, 0 failed"
    );
}

#[test]
fn one_line_ending_comes_off_and_no_more() {
    assert_eq!(strip_one_line_ending("secret\n"), "secret");
    assert_eq!(strip_one_line_ending("secret\r\n"), "secret");
    assert_eq!(strip_one_line_ending("secret"), "secret");
    assert_eq!(strip_one_line_ending("secret\n\n"), "secret\n");
    assert_eq!(strip_one_line_ending("a\nb\n"), "a\nb");
}

/// A carriage return that is not part of a line ending was in the value.
#[test]
fn a_lone_carriage_return_is_left_where_it_is() {
    assert_eq!(strip_one_line_ending("secret\r"), "secret\r");
}

#[test]
fn a_command_is_one_span_naming_its_backend_and_nothing_it_addressed() {
    const TOKEN: &str = "secret-span-token-8c3d";
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();
    let section: crate::secrets::SecretsSection = toml::from_str(&format!(
        r#"
            [backends.work]
            kind = "openbao"
            addr = "http://127.0.0.1:{port}"
            mount = "secret"
            path_prefix = "trg"
            owner = "yordis"
            token = "{TOKEN}"
            timeout_ms = 500
            "#
    ))
    .expect("section");
    let registry = Registry::new(section);
    let command = parse(&["get", "--backend", "work", "--path", "deploy-keys", "--key", "token"]).expect("parse");

    let (code, trace) = crate::telemetry::testing::capture(|| current_thread(command.handle(&registry)));
    assert_eq!(code, ExitCode::Failed, "nothing listens there");

    let span = trace.span("secret get").expect("the command span");
    assert!(matches!(span.status, Status::Error { .. }));
    let attribute = |key: &str| trace.attribute("secret get", key).map(|v| v.to_string());
    assert_eq!(attribute("trg.secrets.backend.name").as_deref(), Some("work"));
    assert_eq!(attribute("trg.secrets.backend.kind").as_deref(), Some("openbao"));
    trace.assert_child_of("GET /v1/{mount}/data/{path}", "secret get");

    assert_never_recorded(&trace, &[TOKEN, "deploy-keys", "yordis"]);
}
