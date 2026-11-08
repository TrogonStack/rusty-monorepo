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
    let e = VarSource::Env {
        env: name.clone(),
        default: None,
    };
    assert_eq!(
        e.resolve_with_env(&FetchedSecrets::new(), &|key| (key == name).then(|| "hi".to_string()))
            .unwrap(),
        "hi"
    );
}

#[test]
fn varsource_env_falls_back_to_default() {
    let name = format!("TRG_VARSRC_UNSET_{}", std::process::id());
    let e = VarSource::Env {
        env: name,
        default: Some("d".into()),
    };
    assert_eq!(e.resolve_with_env(&FetchedSecrets::new(), &|_| None).unwrap(), "d");
}

#[test]
fn varsource_env_missing_without_default_errors() {
    let name = format!("TRG_VARSRC_REQ_{}", std::process::id());
    let e = VarSource::Env {
        env: name.clone(),
        default: None,
    };
    let err = e.resolve_with_env(&FetchedSecrets::new(), &|_| None).unwrap_err();
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

const WITH_FALLBACKS: &str = r#"{ backend = "homelab", path = "exec/github", key = "GH_TOKEN", fallbacks = [
    { backend = "op", ref = "op://Ops/github/GH_TOKEN" },
] }"#;

fn source_of(table: &str) -> Result<RawVarSource, toml::de::Error> {
    #[derive(Deserialize)]
    struct W {
        t: RawVarSource,
    }
    toml::from_str::<W>(&format!("t = {table}")).map(|w| w.t)
}

fn with_fallbacks_source() -> RawVarSource {
    source_of(WITH_FALLBACKS).expect("fallbacks")
}

#[test]
fn varsource_deserializes_a_secret_table_with_fallbacks() {
    let source = with_fallbacks_source();
    assert!(source.reads_secret());
    assert!(source.secret().is_none(), "a chain is not one address");
    let VarSource::SecretWithFallbacks(chain) = source else {
        panic!("expected a secret with fallbacks");
    };
    assert_eq!(chain.primary.backend, "homelab");
    assert_eq!(chain.fallbacks.len(), 1);
    assert_eq!(
        chain.fallbacks[0].reference.as_deref(),
        Some("op://Ops/github/GH_TOKEN")
    );
}

/// An entry is an address and nothing else, so a list inside a list has
/// no way to be written down.
#[test]
fn a_fallback_entry_cannot_carry_fallbacks() {
    let err = source_of(
        r#"{ backend = "a", path = "p", key = "k", fallbacks = [ { backend = "b", path = "p", key = "k", fallbacks = [] } ] }"#,
    )
    .unwrap_err();
    assert!(err.to_string().contains("fallbacks"), "{err}");
}

#[test]
fn fallbacks_resolve_to_a_read_with_its_addresses_in_order() {
    let section = declaring(&[("homelab", "openbao"), ("op", "onepassword")]);
    let resolved = with_fallbacks_source()
        .into_resolved(&section, || VarSite::exec_env("acme", "GH_TOKEN"))
        .expect("resolves");

    let read = resolved.secret_read().expect("a read");
    assert_eq!(read.primary().backend(), "homelab");
    assert_eq!(read.fallbacks().len(), 1);
    assert_eq!(read.fallbacks()[0].backend(), "op");
}

#[test]
fn empty_fallbacks_are_refused_where_the_config_is_read() {
    let source = source_of(r#"{ backend = "a", path = "p", key = "k", fallbacks = [] }"#).unwrap();
    let err = source
        .into_resolved(&declaring(&[]), || VarSite::exec_env("acme", "GH_TOKEN"))
        .unwrap_err();
    assert!(matches!(err, SecretVarError::EmptyFallbacks { .. }), "{err}");
}

#[test]
fn a_fallback_entry_naming_an_undeclared_backend_is_refused() {
    let err = with_fallbacks_source()
        .into_resolved(&declaring(&[("homelab", "openbao")]), || {
            VarSite::exec_env("acme", "GH_TOKEN")
        })
        .unwrap_err();
    assert!(err.to_string().contains("op"), "{err}");
}

#[test]
fn resolved_fallbacks_resolve_from_what_the_chain_fetched() {
    let section = declaring(&[("homelab", "openbao"), ("op", "onepassword")]);
    let resolved = with_fallbacks_source()
        .into_resolved(&section, || VarSite::exec_env("acme", "GH_TOKEN"))
        .unwrap();
    let read = resolved.secret_read().unwrap();

    let mut fetched = FetchedSecrets::new();
    fetched.insert(read, SecretString::from("t".to_string()));

    assert_eq!(resolved.resolve(&fetched).unwrap(), "t");
    assert!(matches!(
        resolved.resolve(&FetchedSecrets::new()).unwrap_err(),
        VarResolveError::SecretNotFetched(_)
    ));
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
    let v = env_value(&format!(r#"v = [{{ env = "{name}" }}, "/app/state"]"#), &[]);
    assert_eq!(
        v.resolve_with_env(&FetchedSecrets::new(), &|key| (key == name)
            .then(|| "/home/tester".to_string()))
            .unwrap(),
        "/home/tester/app/state"
    );
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
    let v = env_value(&format!(r#"v = {{ env = "{name}" }}"#), &[]);
    assert_eq!(
        v.resolve_with_env(&FetchedSecrets::new(), &|key| (key == name).then(|| "hi".to_string()))
            .unwrap(),
        "hi"
    );

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
