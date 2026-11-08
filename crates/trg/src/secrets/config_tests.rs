use super::*;

fn section(toml_text: &str) -> Result<SecretsSection, toml::de::Error> {
    toml::from_str(toml_text)
}

/// A backend cannot be reached in order to learn how to reach it, and the
/// refusal belongs where the config is read rather than at the first use,
/// where it would surface as a cycle instead of a mistake.
#[test]
fn a_backend_cannot_take_its_own_address_from_a_backend() {
    let section: SecretsSection = toml::from_str(
        r#"
            [backends.homelab]
            kind = "openbao"
            addr = { backend = "homelab", path = "self", key = "addr" }
            mount = "kv"
            path_prefix = "trg"
            owner = "yordis"
            token_file = "~/.vault-token"
            "#,
    )
    .unwrap();

    let err = Registry::new(section).resolve("homelab").unwrap_err();
    assert!(matches!(err, BackendError::SecretVar { field: "addr", .. }), "{err}");
}

/// A secret with fallbacks reads a secret just as one address does, so it is no
/// more able to say how to reach a backend than a single address is.
#[test]
fn a_backend_cannot_take_its_address_or_token_from_fallbacks() {
    let list = r#"{ backend = "homelab", path = "self", key = "v", fallbacks = [ { backend = "homelab", path = "self", key = "v" } ] }"#;
    for (field, addr, token) in [
        ("addr", list, r#""t""#),
        ("token", r#""https://bao.example.com""#, list),
    ] {
        let section: SecretsSection = toml::from_str(&format!(
            r#"
                [backends.homelab]
                kind = "openbao"
                addr = {addr}
                mount = "kv"
                path_prefix = "trg"
                owner = "me"
                token = {token}
                "#
        ))
        .unwrap();

        let err = Registry::new(section).resolve("homelab").unwrap_err();
        assert!(
            matches!(err, BackendError::SecretVar { field: f, .. } if f == field),
            "{err}"
        );
    }
}

#[test]
fn only_a_backend_that_cannot_be_resolved_here_lets_a_read_fall_through() {
    let section: SecretsSection = toml::from_str(
        r#"
            [backends.homelab]
            kind = "openbao"
            addr = { env = "TRG_TEST_UNSET_BAO_ADDR_9f1" }
            mount = "kv"
            path_prefix = "trg"
            owner = "me"
            token_file = "~/.t"
            "#,
    )
    .unwrap();
    let registry = Registry::new(section);

    assert!(registry.resolve("homelab").unwrap_err().allows_fallback());
    assert!(!registry.resolve("typo").unwrap_err().allows_fallback());
}

fn openbao_with(addr: &str, extra: &str) -> Registry {
    let section: SecretsSection = toml::from_str(&format!(
        r#"
            [backends.homelab]
            kind = "openbao"
            addr = "{addr}"
            mount = "kv"
            path_prefix = "trg"
            owner = "me"
            token_file = "~/.t"
            {extra}
            "#
    ))
    .unwrap();
    Registry::new(section)
}

#[test]
fn a_ca_cert_file_this_machine_cannot_open_lets_a_read_fall_through() {
    let err = openbao_with(
        "https://bao.example.com",
        r#"ca_cert_file = "/nonexistent/trg-test-ca.pem""#,
    )
    .resolve("homelab")
    .unwrap_err();
    assert!(
        matches!(
            err,
            BackendError::Build {
                cause: OpenBaoBuildError::CaCertRead { .. },
                ..
            }
        ),
        "{err}"
    );
    assert!(err.allows_fallback());
}

#[test]
fn a_malformed_backend_config_still_stops_a_read() {
    let err = openbao_with("ftp://bao.example.com", "")
        .resolve("homelab")
        .unwrap_err();
    assert!(matches!(err, BackendError::Build { .. }), "{err}");
    assert!(!err.allows_fallback(), "{err}");
}

#[test]
fn a_ca_cert_file_that_is_not_a_certificate_still_stops_a_read() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("trg-test-not-a-cert-{}.pem", std::process::id()));
    std::fs::write(&path, "not a certificate").unwrap();
    let err = openbao_with(
        "https://bao.example.com",
        &format!("ca_cert_file = {:?}", path.display().to_string()),
    )
    .resolve("homelab")
    .unwrap_err();
    std::fs::remove_file(&path).ok();
    assert!(matches!(err, BackendError::Build { .. }), "{err}");
    assert!(!err.allows_fallback(), "{err}");
}

#[test]
fn a_backend_cannot_take_its_own_token_from_a_backend() {
    let section: SecretsSection = toml::from_str(
        r#"
            [backends.homelab]
            kind = "openbao"
            addr = "https://bao.example.com"
            mount = "kv"
            path_prefix = "trg"
            owner = "yordis"
            token = { backend = "homelab", path = "self", key = "token" }
            "#,
    )
    .unwrap();

    let err = Registry::new(section).resolve("homelab").unwrap_err();
    assert!(matches!(err, BackendError::SecretVar { field: "token", .. }), "{err}");
    assert!(err.to_string().contains("cannot come from a secrets backend"), "{err}");
}

#[test]
fn keychain_backend_parses_with_and_without_a_service() {
    let s = section(
        r#"
            [backends.local]
            kind = "keychain"

            [backends.named]
            kind = "keychain"
            service = "custom"
            "#,
    )
    .expect("parse");
    assert_eq!(s.backends.len(), 2);
    assert_eq!(s.backends["local"].kind(), BackendKind::Keychain);
}

#[test]
fn onepassword_backend_carries_the_account_it_declares() {
    let s = section(
        r#"
            [backends.named]
            kind = "onepassword"
            account = "my.1password.com"
            "#,
    )
    .expect("parse");
    assert_eq!(s.backends["named"].kind(), BackendKind::OnePassword);

    let Backend::OnePassword(b) = build("named", &s.backends["named"]).expect("build") else {
        panic!("expected a 1Password backend")
    };
    assert_eq!(b.account().as_str(), "my.1password.com");
}

/// Without one, `op` resolves a vault name against whichever account it
/// considers the default, so the same config addresses different vaults on
/// different machines.
#[test]
fn onepassword_without_an_account_is_refused() {
    let err = section(
        r#"
            [backends.local]
            kind = "onepassword"
            "#,
    )
    .expect_err("should refuse");
    assert!(err.to_string().contains("account"), "{err}");
}

#[test]
fn onepassword_rejects_an_account_that_would_read_as_a_flag() {
    let err = section(
        r#"
            [backends.local]
            kind = "onepassword"
            account = "--account"
            "#,
    )
    .expect_err("should refuse");
    assert!(err.to_string().contains("must not start with `-`"), "{err}");
}

#[test]
fn onepassword_rejects_an_unknown_field() {
    let err = section(
        r#"
            [backends.work]
            kind = "onepassword"
            vault = "Ops"
            "#,
    )
    .expect_err("should reject");
    assert!(err.to_string().contains("vault"), "{err}");
}

#[test]
fn openbao_backend_parses_a_full_declaration() {
    let s = section(
        r#"
            [backends.work]
            kind = "openbao"
            addr = { env = "BAO_ADDR" }
            mount = "secret"
            path_prefix = "trg"
            owner = "yordis"
            token_file = "~/.vault-token"
            "#,
    )
    .expect("parse");
    assert_eq!(s.backends["work"].kind(), BackendKind::Openbao);
}

#[test]
fn an_unknown_kind_names_the_kinds_that_exist() {
    let err = section(
        r#"
            [backends.work]
            kind = "hashivault"
            "#,
    )
    .expect_err("should reject");
    let rendered = err.to_string();
    assert!(rendered.contains("keychain"), "{rendered}");
    assert!(rendered.contains("openbao"), "{rendered}");
}

#[test]
fn an_unknown_field_is_rejected_rather_than_ignored() {
    let err = section(
        r#"
            [backends.work]
            kind = "openbao"
            addr = "https://bao:8200"
            mount = "secret"
            path_prefix = "trg"
            owner = "yordis"
            token_file = "~/.vault-token"
            tls_skip_verify = true
            "#,
    )
    .expect_err("should reject");
    assert!(err.to_string().contains("tls_skip_verify"), "{err}");
}

#[test]
fn a_backend_cannot_read_a_secret_to_reach_the_secret_store() {
    let err = section(
        r#"
            [backends.work]
            kind = "openbao"
            addr = { secret = "bootstrap#addr" }
            mount = "secret"
            path_prefix = "trg"
            owner = "yordis"
            token_file = "~/.vault-token"
            "#,
    )
    .expect_err("should reject a secret reference in a backend declaration");
    assert!(err.to_string().contains("env"), "{err}");
}

#[test]
fn openbao_requires_addr_mount_and_path_prefix() {
    for missing in ["addr", "mount", "path_prefix"] {
        let mut lines = vec![
            "[backends.work]".to_string(),
            "kind = \"openbao\"".to_string(),
            "token_file = \"~/.vault-token\"".to_string(),
        ];
        for (field, value) in [
            ("addr", "\"http://bao:8200\""),
            ("mount", "\"secret\""),
            ("path_prefix", "\"trg\""),
        ] {
            if field != missing {
                lines.push(format!("{field} = {value}"));
            }
        }
        let err = section(&lines.join("\n")).expect_err("should require {missing}");
        assert!(err.to_string().contains(missing), "missing {missing}: {err}");
    }
}

/// The whole point of the field: a config that leaves the ACL boundary
/// unstated must not load. Serde names the field, which is the shortest
/// route from the error to the line that has to change.
#[test]
fn a_declaration_without_an_owner_is_refused_rather_than_shared() {
    let err = section(
        r#"
            [backends.work]
            kind = "openbao"
            addr = "https://bao:8200"
            mount = "secret"
            path_prefix = "trg"
            token_file = "~/.vault-token"
            "#,
    )
    .expect_err("an openbao backend without an owner should not parse");

    assert!(err.to_string().contains("owner"), "{err}");
}

/// `owner` is one path segment, not a subtree, so a slash in it would
/// silently widen what a templated ACL path has to match.
#[test]
fn an_owner_that_is_not_a_single_segment_is_refused_at_config_time() {
    for owner in ["a/b", "..", ".", "", "yor dis"] {
        let s = section(&format!(
            r#"
                [backends.work]
                kind = "openbao"
                addr = "https://bao:8200"
                mount = "secret"
                path_prefix = "trg"
                owner = "{owner}"
                token_file = "~/.vault-token"
                "#
        ))
        .expect("parse");

        let err = build("work", &s.backends["work"]).expect_err("should refuse {owner:?}");
        assert!(err.to_string().contains("owner"), "{owner:?}: {err}");
    }
}

#[test]
fn a_machine_id_is_optional_and_is_not_derived_from_the_owner() {
    let s = section(
        r#"
            [backends.work]
            kind = "openbao"
            addr = "https://bao:8200"
            mount = "secret"
            path_prefix = "trg"
            owner = "yordis"
            token_file = "~/.vault-token"
            "#,
    )
    .expect("parse");

    let Backend::OpenBao(b) = build("work", &s.backends["work"]).expect("build") else {
        panic!("expected an openbao backend")
    };
    assert_eq!(b.machine_id(), None);
    assert_eq!(b.credential_path("github"), "mcp/github");
}

/// A bad prefix has to fail while the user is looking at their config, not
/// on the first secret operation long after it loaded.
#[test]
fn a_prefix_that_cannot_address_anything_is_refused_at_config_time() {
    for prefix in ["trg//mcp", "trg/../mcp", "trg/./mcp", "trg/ mcp"] {
        let s = section(&format!(
            r#"
                [backends.work]
                kind = "openbao"
                addr = "https://bao:8200"
                mount = "secret"
                path_prefix = "{prefix}"
                owner = "yordis"
                token_file = "~/.vault-token"
                "#
        ))
        .expect("parse");

        assert!(
            matches!(
                build("work", &s.backends["work"]),
                Err(BackendError::Build {
                    cause: OpenBaoBuildError::Segment {
                        field: "path_prefix",
                        ..
                    },
                    ..
                })
            ),
            "should refuse the prefix {prefix:?} at config time"
        );
    }
}

#[test]
fn a_slash_wrapped_prefix_is_the_same_prefix() {
    let s = section(
        r#"
            [backends.work]
            kind = "openbao"
            addr = "https://bao:8200"
            mount = "secret"
            path_prefix = "/trg/mcp/"
            owner = "yordis"
            token_file = "~/.vault-token"
            "#,
    )
    .expect("parse");
    assert!(build("work", &s.backends["work"]).is_ok());
}

/// Nothing is derived from the host any more, so nothing can silently
/// collide with another machine that happens to share a short hostname.
#[test]
fn an_omitted_machine_id_stays_omitted_rather_than_being_guessed() {
    let s = section(
        r#"
            [backends.work]
            kind = "openbao"
            addr = "https://bao:8200"
            mount = "secret"
            path_prefix = "trg"
            owner = "yordis"
            token_file = "~/.vault-token"
            "#,
    )
    .expect("parse");

    let Backend::OpenBao(b) = build("work", &s.backends["work"]).expect("build") else {
        panic!("expected an openbao backend")
    };
    assert_eq!(b.machine_id(), None);
    assert_eq!(b.credential_path("github"), "mcp/github");
}

#[test]
fn a_declared_machine_id_still_has_to_be_addressable() {
    let s = section(
        r#"
            [backends.work]
            kind = "openbao"
            addr = "https://bao:8200"
            mount = "secret"
            path_prefix = "trg"
            owner = "yordis"
            machine_id = "my laptop"
            token_file = "~/.vault-token"
            "#,
    )
    .expect("parse");

    assert!(matches!(
        build("work", &s.backends["work"]),
        Err(BackendError::Build {
            cause: OpenBaoBuildError::Segment {
                field: "machine_id",
                ..
            },
            ..
        })
    ));
}

#[test]
fn an_empty_path_prefix_is_allowed() {
    let s = section(
        r#"
            [backends.work]
            kind = "openbao"
            addr = "https://bao:8200"
            mount = "secret"
            path_prefix = ""
            owner = "yordis"
            token_file = "~/.vault-token"
            "#,
    )
    .expect("parse");
    let backend = build("work", &s.backends["work"]).expect("build");
    assert_eq!(backend.kind(), "openbao");
}

#[test]
fn exactly_one_token_source_is_required() {
    let neither = section(
        r#"
            [backends.work]
            kind = "openbao"
            addr = "https://bao:8200"
            mount = "secret"
            path_prefix = "trg"
            owner = "yordis"
            "#,
    )
    .expect("parse");
    assert!(matches!(
        build("work", &neither.backends["work"]),
        Err(BackendError::TokenSource { found: "neither", .. })
    ));

    let both = section(
        r#"
            [backends.work]
            kind = "openbao"
            addr = "https://bao:8200"
            mount = "secret"
            path_prefix = "trg"
            owner = "yordis"
            token_file = "~/.vault-token"
            token = { env = "BAO_TOKEN" }
            "#,
    )
    .expect("parse");
    assert!(matches!(
        build("work", &both.backends["work"]),
        Err(BackendError::TokenSource { found: "both", .. })
    ));
}

#[test]
fn an_unresolvable_addr_names_the_field_and_the_backend() {
    let s = section(
        r#"
            [backends.work]
            kind = "openbao"
            addr = { env = "TRG_TEST_DEFINITELY_UNSET_BAO_ADDR" }
            mount = "secret"
            path_prefix = "trg"
            owner = "yordis"
            token_file = "~/.vault-token"
            "#,
    )
    .expect("parse");
    let err = build("work", &s.backends["work"]).expect_err("should fail");
    let rendered = err.to_string();
    assert!(rendered.contains("secrets.backends.work"), "{rendered}");
    assert!(rendered.contains("addr"), "{rendered}");
}

#[test]
fn resolving_an_undeclared_name_lists_what_is_declared() {
    let registry = Registry::new(
        section(
            r#"
                [backends.local]
                kind = "keychain"

                [backends.work]
                kind = "keychain"
                "#,
        )
        .expect("parse"),
    );

    let err = registry.resolve("nope").expect_err("should fail");
    let rendered = err.to_string();
    assert!(rendered.contains("local, work"), "{rendered}");
}

#[test]
fn resolving_against_no_declarations_names_the_section_to_add() {
    let registry = Registry::default();
    let err = registry.resolve("work").expect_err("should fail");
    assert!(err.to_string().contains("[secrets.backends.work]"), "{err}");
}

#[test]
fn a_server_that_names_no_backend_keeps_the_pre_secrets_keychain() {
    let registry = Registry::new(
        section(
            r#"
                [backends.work]
                kind = "openbao"
                addr = "https://bao.example.com:8200"
                mount = "secret"
                path_prefix = "trg"
                owner = "yordis"
                token_file = "~/.vault-token"
                "#,
        )
        .expect("parse"),
    );

    let backend = registry.for_server("github", None).expect("default");
    assert_eq!(backend.kind(), "keychain");
}

#[test]
fn a_server_gets_exactly_the_backend_it_names() {
    let registry = Registry::new(
        section(
            r#"
                [backends.local]
                kind = "keychain"

                [backends.work]
                kind = "openbao"
                addr = "https://bao.example.com:8200"
                mount = "secret"
                path_prefix = "trg"
                owner = "yordis"
                machine_id = "laptop"
                token_file = "~/.vault-token"
                "#,
        )
        .expect("parse"),
    );

    assert_eq!(
        registry.for_server("a", Some("local")).expect("local").kind(),
        "keychain"
    );
    assert_eq!(registry.for_server("b", Some("work")).expect("work").kind(), "openbao");
}

#[test]
fn a_server_naming_an_undeclared_backend_names_both_in_the_error() {
    let registry = Registry::new(
        section(
            r#"[backends.local]
            kind = "keychain""#,
        )
        .expect("parse"),
    );

    let err = registry.for_server("github", Some("work")).expect_err("should fail");
    let message = err.to_string();

    assert!(message.contains("mcp.servers.github"), "{message}");
    assert!(message.contains("work"), "{message}");
    assert!(message.contains("local"), "{message}");
}

#[test]
fn the_default_backend_is_the_keychain_service_used_before_secrets_existed() {
    let backend = Registry::default_backend();
    assert_eq!(backend.kind(), "keychain");
    assert!(backend.describe().contains(DEFAULT_SERVICE), "{}", backend.describe());
}
