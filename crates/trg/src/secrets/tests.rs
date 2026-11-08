use super::*;

#[test]
fn path_rejects_structural_problems() {
    assert!(matches!(SecretPath::parse(""), Err(PathError::Empty)));
    assert!(matches!(SecretPath::parse("/a"), Err(PathError::Slash(_))));
    assert!(matches!(SecretPath::parse("a/"), Err(PathError::Slash(_))));
    assert!(matches!(SecretPath::parse("a//b"), Err(PathError::EmptySegment(_))));
    assert!(matches!(SecretPath::parse("a/../b"), Err(PathError::DotSegment(_))));
    assert!(matches!(SecretPath::parse("a/./b"), Err(PathError::DotSegment(_))));
    assert!(matches!(SecretPath::parse("a#b"), Err(PathError::Hash(_))));
}

#[test]
fn path_accepts_what_a_server_name_can_be() {
    // Server names are TOML table keys today, so spaces and dots are legal
    // and Phase 1 must not start rejecting them.
    for raw in ["github", "my server", "a.b", "mcp/github", "Ünïcode"] {
        assert!(SecretPath::parse(raw).is_ok(), "should accept {raw:?}");
    }
}

#[test]
fn path_prefix_join() {
    let p = SecretPath::parse("mcp/github").expect("parse");
    assert_eq!(p.with_prefix("trg"), "trg/mcp/github");
    assert_eq!(p.with_prefix(""), "mcp/github");
}

#[test]
fn key_rejects_empty_and_hash() {
    assert!(matches!(SecretKey::parse(""), Err(KeyError::Empty)));
    assert!(matches!(SecretKey::parse("a#b"), Err(KeyError::Hash(_))));
    assert!(SecretKey::parse("token").is_ok());
}

#[test]
fn secret_ref_parses_path_and_key() {
    let r = SecretRef::parse("mcp/github#token").expect("parse");
    assert_eq!(r.path().as_str(), "mcp/github");
    assert_eq!(r.key().as_str(), "token");
    assert_eq!(r.to_string(), "mcp/github#token");
}

#[test]
fn secret_ref_rejects_bad_shapes() {
    assert!(matches!(SecretRef::parse("nohash"), Err(RefError::Shape(_))));
    assert!(matches!(SecretRef::parse("a#b#c"), Err(RefError::Shape(_))));
    assert!(matches!(SecretRef::parse("#key"), Err(RefError::Path { .. })));
    assert!(matches!(SecretRef::parse("path#"), Err(RefError::Key { .. })));
}

#[test]
fn secret_map_debug_redacts_every_value() {
    let mut map = SecretMap::new();
    map.insert(SecretKey::parse("token").unwrap(), SecretString::from("hunter2"));
    map.insert(SecretKey::parse("other").unwrap(), SecretString::from("s3cret"));

    let rendered = format!("{map:?}");
    assert!(!rendered.contains("hunter2"), "leaked a value: {rendered}");
    assert!(!rendered.contains("s3cret"), "leaked a value: {rendered}");
    assert!(rendered.contains("token"), "should name keys: {rendered}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
}

/// `Malformed.raw` is the payload itself, and for a credential path that
/// is live tokens. Nothing here renders it through `Display`, but the
/// derived `Debug` would if this field were a plain `String`; this pins
/// [`secrecy::SecretString`] as the reason it does not.
#[test]
fn malformed_debug_redacts_the_raw_payload() {
    let err = SecretsError::Malformed {
        path: SecretPath::parse("github").unwrap(),
        cause: "injected".to_string(),
        raw: Some(SecretString::from("hunter2-access-token".to_string())),
    };

    let rendered = format!("{err:?}");
    assert!(
        !rendered.contains("hunter2-access-token"),
        "leaked the payload: {rendered}"
    );
}

#[test]
fn secret_map_json_roundtrip() {
    let mut map = SecretMap::new();
    map.insert(SecretKey::parse("a").unwrap(), SecretString::from("1"));
    map.insert(SecretKey::parse("b").unwrap(), SecretString::from("2"));

    let json = map.to_json().expect("encode");
    let back = SecretMap::from_json(&json).expect("decode");

    assert_eq!(back.len(), 2);
    assert_eq!(
        back.get(&SecretKey::parse("a").unwrap()).map(|v| v.expose_secret()),
        Some("1")
    );
    assert_eq!(
        back.get(&SecretKey::parse("b").unwrap()).map(|v| v.expose_secret()),
        Some("2")
    );
}

#[test]
fn a_keychain_credential_path_is_the_server_name_verbatim() {
    let backend = Backend::Keychain(KeychainBackend::with_default_service());
    assert_eq!(backend.credential_path("github").expect("path").as_str(), "github");
}

#[test]
fn an_openbao_credential_path_is_scoped_per_machine() {
    let backend = Backend::OpenBao(
        openbao::OpenBaoBackend::new(openbao::OpenBaoSettings {
            addr: "https://bao.example.com:8200".to_string(),
            mount: "secret".to_string(),
            path_prefix: "trg".to_string(),
            owner: "yordis".to_string(),
            machine_id: Some("laptop".to_string()),
            token: openbao::TokenSource::Var(crate::config::VarSource::Literal("t".to_string())),
            ca_cert_file: None,
            timeout: std::time::Duration::from_secs(5),
        })
        .expect("build"),
    );

    assert_eq!(
        backend.credential_path("github").expect("path").as_str(),
        "mcp/laptop/github"
    );

    let err = backend.credential_path("my server").expect_err("should refuse");
    assert!(err.to_string().contains("openbao"), "{err}");
}

fn openbao_backend(machine_id: Option<&str>) -> Backend {
    Backend::OpenBao(
        openbao::OpenBaoBackend::new(openbao::OpenBaoSettings {
            addr: "https://bao.example.com:8200".to_string(),
            mount: "secret".to_string(),
            path_prefix: "trg".to_string(),
            owner: "yordis".to_string(),
            machine_id: machine_id.map(str::to_string),
            token: openbao::TokenSource::Var(crate::config::VarSource::Literal("t".to_string())),
            ca_cert_file: None,
            timeout: std::time::Duration::from_secs(5),
        })
        .expect("build"),
    )
}

#[test]
fn an_openbao_backend_with_a_machine_id_offers_the_shared_path_as_a_fallback() {
    let backend = openbao_backend(Some("laptop"));
    assert_eq!(
        backend.shared_credential_path("github").expect("path"),
        Some(SecretPath::parse("mcp/github").expect("parse"))
    );
}

#[test]
fn an_openbao_backend_without_a_machine_id_has_no_fallback_to_offer() {
    let backend = openbao_backend(None);
    assert_eq!(backend.shared_credential_path("github").expect("path"), None);
}

#[test]
fn an_unaddressable_server_name_is_refused_the_same_way_for_the_shared_path() {
    let backend = openbao_backend(Some("laptop"));
    let err = backend.shared_credential_path("my server").expect_err("should refuse");
    assert!(err.to_string().contains("openbao"), "{err}");
}

/// Only OpenBao has an out-of-band removal command to give; the Keychain
/// and 1Password are removed through their own tools, not a command `trg`
/// prints, so they answer `None` rather than guessing one.
#[test]
fn only_openbao_offers_a_removal_command() {
    let path = SecretPath::parse("mcp/github").expect("parse");

    let openbao = openbao_backend(Some("laptop"));
    assert_eq!(
        openbao.removal_command(&path),
        Some("bao kv metadata delete secret/trg/yordis/mcp/github".to_string())
    );

    let keychain = Backend::Keychain(KeychainBackend::with_default_service());
    assert_eq!(keychain.removal_command(&path), None);
}

#[test]
fn a_onepassword_credential_path_is_refused_for_lack_of_a_vault() {
    let backend = Backend::OnePassword(OnePasswordBackend::new(
        OpAccount::parse("my.1password.com").expect("account"),
    ));
    let err = backend.credential_path("github").expect_err("should refuse");
    assert!(
        matches!(
            err,
            CredentialPathError::Name {
                kind: "onepassword",
                ..
            }
        ),
        "{err}"
    );
}

#[tokio::test]
async fn fake_backend_roundtrips_through_the_enum() {
    let backend = Backend::Fake(fake::FakeBackend::new());
    let path = SecretPath::parse("mcp/github").expect("parse");
    assert_eq!(backend.kind(), "fake");
    assert!(backend.get(&path).await.expect("get").is_none());

    let mut map = SecretMap::new();
    map.insert(SecretKey::parse("token").unwrap(), SecretString::from("v"));
    backend.set(&path, &map).await.expect("set");

    let loaded = backend.get(&path).await.expect("get").expect("some");
    assert_eq!(
        loaded
            .get(&SecretKey::parse("token").unwrap())
            .map(|v| v.expose_secret()),
        Some("v")
    );

    backend.delete(&path).await.expect("delete");
    assert!(backend.get(&path).await.expect("get").is_none());
}
