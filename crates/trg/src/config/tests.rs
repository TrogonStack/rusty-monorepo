mod doc_examples_tests;

use crate::secrets::telemetry::testing::assert_never_recorded;
use secrecy::ExposeSecret as _;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

use super::*;
use crate::secrets::keychain::KeychainReference;
use crate::secrets::{SecretAddress, SecretKey, SecretPath};
use tempfile::tempdir;

fn keychain_var(backend: &str, path: &str, key: &str) -> SecretVar {
    SecretVar::new(
        backend.to_string(),
        SecretAddress::Keychain(KeychainReference::new(
            SecretPath::parse(path).expect("path"),
            SecretKey::parse(key).expect("key"),
        )),
    )
}

/// A config that exists but will not open must not be reported as one that
/// was never written, since the two have opposite remedies.
#[test]
fn a_config_that_cannot_be_read_is_not_reported_as_missing() {
    let dir = tempdir().unwrap();

    let missing = read_error(
        &dir.path().join("never-written.toml"),
        std::io::Error::from(std::io::ErrorKind::NotFound),
    );
    assert!(matches!(missing, ConfigError::NotFound(_)), "{missing}");

    // A directory opens and then fails to read, which is the shape of every
    // non-`NotFound` failure here.
    let unreadable = std::fs::read_to_string(dir.path()).expect_err("a directory is not readable");
    let err = read_error(dir.path(), unreadable);
    assert!(matches!(err, ConfigError::Unreadable { .. }), "{err}");
    assert!(err.to_string().contains("could not be read"), "{err}");
}

fn write_secure_config(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn load_at_with_env(
    path: &Path,
    server: &str,
    environment: &impl Fn(&str) -> Option<String>,
) -> Result<ResolvedMcpServer, ConfigError> {
    load_mcp_at(path, server)?
        .finish_with_env(&FetchedSecrets::new(), environment)
        .map(|loaded| loaded.server)
}

fn load_at(path: &Path, server: &str) -> Result<ResolvedMcpServer, ConfigError> {
    load_full(path, server).map(|loaded| loaded.server)
}

fn load_full(path: &Path, server: &str) -> Result<LoadedMcp, ConfigError> {
    load_mcp_at(path, server)?.finish(&FetchedSecrets::new())
}

#[test]
fn a_server_carries_the_backend_it_names_and_the_secrets_section_beside_it() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
        [secrets.backends.work]
        kind = "openbao"
        addr = "https://bao.example.com:8200"
        mount = "secret"
        path_prefix = "trg"
        owner = "yordis"
        machine_id = "laptop"
        token_file = "~/.vault-token"

        [mcp.servers.s1]
        url = "https://example.com/mcp"
        secrets = "work"

        [mcp.servers.s2]
        url = "https://example.com/mcp"
        "#,
    );

    let loaded = load_full(&path, "s1").unwrap();
    assert_eq!(loaded.server.secrets.as_deref(), Some("work"));
    assert!(loaded.secrets.backends.contains_key("work"));

    let loaded = load_full(&path, "s2").unwrap();
    assert_eq!(loaded.server.secrets, None);
    assert!(
        loaded.secrets.backends.contains_key("work"),
        "the section travels with every server, so `[mcp]` and `[secrets]` cannot drift"
    );
}

/// Parsing must not depend on a reachable backend: a config with a syntax
/// error has to read as one on a machine that is offline or holding an
/// expired token, which is exactly when someone is editing it.
#[test]
fn a_secret_var_is_declared_at_parse_time_and_read_later() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
        [secrets.backends.homelab]
        kind = "keychain"

        [mcp.servers.s1]
        url = "https://example.com/mcp"

        [mcp.servers.s1.headers]
        Authorization = { var = "token" }

        [mcp.servers.s1.vars]
        token = { backend = "homelab", path = "agentgateway", key = "token" }
        "#,
    );

    let pending = load_mcp_at(&path, "s1").expect("parses without reaching anything");
    assert_eq!(
        pending.secret_vars(),
        vec![keychain_var("homelab", "agentgateway", "token").into()]
    );

    let mut fetched = FetchedSecrets::new();
    fetched.insert(pending.secret_vars()[0].clone(), SecretString::from("t".to_string()));

    let loaded = pending.finish(&fetched).unwrap();
    assert_eq!(
        loaded.server.http_headers[&HeaderName::from_static("authorization")].expose_secret(),
        "t"
    );
}

/// A typo in a backend name has to read as the config mistake it is, and
/// it has to do so wherever it sits: launching `s1` must not hide that
/// `s2` or an unrelated exec entry can never run.
#[test]
fn a_var_naming_an_undeclared_backend_anywhere_in_the_document_fails_the_load() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
        [secrets.backends.homelab]
        kind = "keychain"

        [mcp.servers.s1]
        url = "https://example.com/mcp"

        [mcp.servers.s1.vars]
        a = { backend = "homelab", path = "agentgateway", key = "token" }

        [mcp.servers.s2]
        url = "https://example.com/other"

        [mcp.servers.s2.vars]
        b = { backend = "hoemlab", path = "agentgateway", key = "token" }

        [exec.p]
        command = "claude"

        [exec.p.env]
        TOKEN = { backend = "nowhere", path = "agentgateway", key = "token" }
        "#,
    );

    let message = load_mcp_at(&path, "s1")
        .expect_err("the document does not load")
        .to_string();
    assert!(
        message.contains("`hoemlab`") || message.contains("`nowhere`"),
        "{message}"
    );
    assert!(message.contains("declared: homelab"), "{message}");

    let message = load_exec_at(&path, "p")
        .expect_err("nor from the exec side")
        .to_string();
    assert!(
        message.contains("[exec.p.env] TOKEN names backend `nowhere`"),
        "{message}"
    );
}

#[test]
fn one_entry_read_by_two_vars_is_named_once() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
        [secrets.backends.homelab]
        kind = "keychain"

        [mcp.servers.s1]
        url = "https://example.com/mcp"

        [mcp.servers.s1.vars]
        a = { backend = "homelab", path = "agentgateway", key = "token" }
        b = { backend = "homelab", path = "agentgateway", key = "token" }
        c = { backend = "homelab", path = "agentgateway", key = "principal" }
        d = "literal"
        "#,
    );

    let wanted = load_mcp_at(&path, "s1").unwrap().secret_vars();

    assert_eq!(
        wanted.len(),
        2,
        "the duplicate collapses and the literal is not a backend read: {wanted:?}"
    );
    assert_eq!(
        wanted[0],
        SecretRead::from(keychain_var("homelab", "agentgateway", "principal")),
        "sorted, so the order does not depend on hashing"
    );
    assert_eq!(
        wanted[1],
        SecretRead::from(keychain_var("homelab", "agentgateway", "token"))
    );
}

#[test]
fn a_server_declaring_no_secret_vars_wants_nothing_read() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
        [mcp.servers.s1]
        url = "https://example.com/mcp"

        [mcp.servers.s1.vars]
        a = "literal"
        "#,
    );

    assert!(load_mcp_at(&path, "s1").unwrap().secret_vars().is_empty());
}

#[test]
fn a_config_without_a_secrets_section_still_loads() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
        [mcp.servers.s1]
        url = "https://example.com/mcp"
        "#,
    );

    let loaded = load_mcp_at(&path, "s1").unwrap();
    assert!(loaded.secrets.backends.is_empty());
}

#[test]
fn a_mirror_that_could_never_apply_is_a_config_error_for_every_load() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
        [secrets.backends.op]
        kind = "onepassword"
        account = "my.1password.com"

        [secrets.mirrors.signing-key]
        source = { backend = "op", ref = "op://Engineering/deploy-key/token" }
        targets = [{ backend = "op", ref = "op://Engineering/other-key/token" }]

        [mcp.servers.s1]
        url = "https://example.com/mcp"
        "#,
    );

    for err in [
        load_mcp_at(&path, "s1").err().unwrap(),
        load_secrets_at(&path).err().unwrap(),
    ] {
        assert!(matches!(err, ConfigError::Mirror(_)), "{err}");
        assert!(err.to_string().contains("[secrets.mirrors.signing-key]"), "{err}");
    }
}

#[test]
fn a_named_secret_that_could_never_be_read_is_a_config_error_for_every_load() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
        [secrets.backends.op]
        kind = "onepassword"
        account = "my.1password.com"

        [secrets.vars.signing-key]
        backend = "ghost"
        path = "git/signing"
        key = "KEY"

        [mcp.servers.s1]
        url = "https://example.com/mcp"
        "#,
    );

    for err in [
        load_mcp_at(&path, "s1").err().unwrap(),
        load_secrets_at(&path).err().unwrap(),
    ] {
        assert!(matches!(err, ConfigError::SecretVar(_)), "{err}");
        assert!(err.to_string().contains("[secrets.vars.signing-key]"), "{err}");
    }
}

#[test]
fn load_rejects_empty_url() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
[mcp.servers.blank]
url = ""
"#,
    );
    assert!(matches!(load_at(&path, "blank").unwrap_err(), ConfigError::EmptyUrl));
}

#[test]
fn load_rejects_whitespace_only_url() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
[mcp.servers.blank]
url = "   "
"#,
    );
    assert!(matches!(load_at(&path, "blank").unwrap_err(), ConfigError::EmptyUrl));
}

#[test]
fn load_rejects_duplicate_header_after_canonicalization() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
[mcp.servers.u]
url = "https://ok"

[mcp.servers.u.headers]
Authorization = "Bearer one"
authorization = "Bearer two"
"#,
    );
    let e = load_at(&path, "u").unwrap_err();
    let ConfigError::DuplicateHeader { ref name, ref existing } = e else {
        panic!("expected DuplicateHeader, got {e:?}");
    };
    let pair = [name.as_str(), existing.as_str()];
    assert!(
        pair.contains(&"Authorization") && pair.contains(&"authorization"),
        "{pair:?}"
    );
}

#[test]
fn load_rejects_empty_resolved_header() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
[mcp.servers.u]
url = "https://ok"

[mcp.servers.u.headers]
X = ""
"#,
    );
    let e = load_at(&path, "u").unwrap_err();
    assert!(
        matches!(
            e,
            ConfigError::InvalidHeaderValue {
                ref name,
                ref cause,
            } if name == "X" && matches!(cause, HeaderValueError::Empty)
        ),
        "{e:?}"
    );
}

#[test]
fn load_rejects_whitespace_only_header() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
[mcp.servers.u]
url = "https://ok"

[mcp.servers.u.headers]
X = "   "
"#,
    );
    let e = load_at(&path, "u").unwrap_err();
    assert!(
        matches!(e, ConfigError::InvalidHeaderValue { ref name, .. } if name == "X"),
        "{e:?}"
    );
}

#[test]
fn load_rejects_empty_url_when_env_default_empty() {
    let key = String::from("EMPTY_URL");
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let cfg = format!(
        r#"
[mcp.servers.u]
url = {{ var = "u" }}

[mcp.servers.u.vars]
u = {{ env = "{key}", default = "" }}
"#,
        key = key,
    );
    write_secure_config(&path, &cfg);
    assert!(matches!(
        load_at_with_env(&path, "u", &|_| None).unwrap_err(),
        ConfigError::EmptyUrl
    ));
}

#[test]
fn load_resolves_literal_header() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
[mcp.servers.alpha]
url = "https://example.com/mcp"

[mcp.servers.alpha.headers]
Authorization = "Bearer secret123"
"#,
    );
    let r = load_at(&path, "alpha").unwrap();
    let auth = http::HeaderName::from_static("authorization");
    assert_eq!(r.http_headers[&auth].expose_secret(), "Bearer secret123");
}

#[test]
fn load_header_from_env_var_source() {
    let key = String::from("TRG_TOK_ENV");
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let cfg = format!(
        r#"
[mcp.servers.e]
url = "https://e.example/mcp"

[mcp.servers.e.vars]
auth = {{ env = "{key}" }}

[mcp.servers.e.headers]
Authorization = {{ var = "auth" }}
"#,
        key = key,
    );
    write_secure_config(&path, &cfg);
    let r = load_at_with_env(&path, "e", &|name| {
        (name == key).then(|| "Bearer from-environment".to_string())
    })
    .unwrap();
    let auth = http::HeaderName::from_static("authorization");
    assert_eq!(r.http_headers[&auth].expose_secret(), "Bearer from-environment");
}

#[test]
fn load_header_env_uses_default_when_unset() {
    let key = String::from("TRG_TOK_ABSENT");
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let cfg = format!(
        r#"
[mcp.servers.d]
url = "https://d.example/mcp"

[mcp.servers.d.vars]
auth = {{ env = "{key}", default = "fallback-val" }}

[mcp.servers.d.headers]
Authorization = {{ var = "auth" }}
"#,
        key = key,
    );
    write_secure_config(&path, &cfg);
    let r = load_at_with_env(&path, "d", &|_| None).unwrap();
    let auth = http::HeaderName::from_static("authorization");
    assert_eq!(r.http_headers[&auth].expose_secret(), "fallback-val");
}

#[test]
fn load_fails_when_header_env_required_but_unset() {
    let key = String::from("TRG_TOK_REQUIRED");
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let cfg = format!(
        r#"
[mcp.servers.x]
url = "https://x.example/mcp"

[mcp.servers.x.vars]
auth = {{ env = "{key}" }}

[mcp.servers.x.headers]
Authorization = {{ var = "auth" }}
"#,
        key = key,
    );
    write_secure_config(&path, &cfg);
    let e = load_at_with_env(&path, "x", &|_| None).unwrap_err();
    assert!(matches!(e, ConfigError::VarResolve(_)), "{e:?}");
}

#[test]
fn unknown_server_lists_available_names() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
[mcp.servers.zebra]
url = "https://z"

[mcp.servers.alpha]
url = "https://a"
"#,
    );
    let err = load_at(&path, "missing").unwrap_err();
    let ConfigError::UnknownServer {
        ref name,
        ref available,
    } = err
    else {
        panic!("unexpected err: {err:?}");
    };
    assert_eq!(name, "missing");
    let listed: HashSet<&str> = available.split(", ").collect();
    assert_eq!(listed, HashSet::from(["alpha", "zebra"]));
}

#[test]
fn load_rejects_unknown_root_toml_key() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(&path, "key = 1\n");
    let err = load_at(&path, "x").unwrap_err();
    assert!(matches!(err, ConfigError::Toml(_)), "{err:?}");
}

#[test]
fn load_without_mcp_table_yields_no_mcp_servers() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(&path, "");
    let err = load_at(&path, "x").unwrap_err();
    assert!(matches!(err, ConfigError::NoMcpServers), "{err:?}");
}

#[test]
fn load_rejects_empty_servers_table() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
[mcp]
"#,
    );
    let err = load_at(&path, "x").unwrap_err();
    assert!(matches!(err, ConfigError::NoMcpServers));
}

#[test]
fn load_accepts_dollar_sequence_in_literal_url() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
[mcp.servers.s1]
url = "https://bad${x}"
"#,
    );
    let r = load_at(&path, "s1").unwrap();
    assert_eq!(r.url.expose_secret(), "https://bad${x}");
}

#[test]
fn load_accepts_dollar_sequence_in_resolved_env_url() {
    let key = String::from("TRG_BAD_URL");
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let cfg = format!(
        r#"
[mcp.servers.badurl]
url = {{ var = "u" }}

[mcp.servers.badurl.vars]
u = {{ env = "{key}" }}
"#,
        key = key,
    );
    write_secure_config(&path, &cfg);
    let r = load_at_with_env(&path, "badurl", &|name| {
        (name == key).then(|| "https://bad${oops}".to_string())
    })
    .unwrap();
    assert_eq!(r.url.expose_secret(), "https://bad${oops}");
}

#[test]
fn load_url_from_env_var_source() {
    let key = String::from("TRG_MCP_URL");
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let cfg = format!(
        r#"
[mcp.servers.uenv]
url = {{ var = "u" }}

[mcp.servers.uenv.vars]
u = {{ env = "{key}" }}
"#,
        key = key,
    );
    write_secure_config(&path, &cfg);
    let r = load_at_with_env(&path, "uenv", &|name| {
        (name == key).then(|| "https://from-env.example/mcp".to_string())
    })
    .unwrap();
    assert_eq!(r.url.expose_secret(), "https://from-env.example/mcp");
}

#[test]
fn load_url_literal_without_placeholders_loads_ok() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
[mcp.servers.ui]
url = "https://svc.example.org/mcp"
"#,
    );
    let r = load_at(&path, "ui").unwrap();
    assert_eq!(r.url.expose_secret(), "https://svc.example.org/mcp");
}

#[test]
fn load_accepts_dollar_sequence_in_plain_header() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
[mcp.servers.u]
url = "https://ok"

[mcp.servers.u.headers]
Authorization = "Bearer ${wrong}"
"#,
    );
    let r = load_at(&path, "u").unwrap();
    let auth = http::HeaderName::from_static("authorization");
    assert_eq!(r.http_headers[&auth].expose_secret(), "Bearer ${wrong}");
}

#[test]
fn load_rejects_unknown_server_field() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
[mcp.servers.s1]
url = "https://ok"
resource = "oops"
"#,
    );
    assert!(matches!(load_at(&path, "s1").unwrap_err(), ConfigError::Toml(_)));
}

#[test]
fn load_accepts_vars_table_with_literal() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
[mcp.servers.v]
url = { var = "endpoint" }

[mcp.servers.v.vars]
endpoint = "https://lit.example/x"
"#,
    );
    let r = load_at(&path, "v").unwrap();
    assert_eq!(r.url.expose_secret(), "https://lit.example/x");
}

#[test]
fn load_rejects_inline_env_in_url() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
[mcp.servers.s]
url = { env = "ANYTHING" }
"#,
    );
    let err = load_at(&path, "s").unwrap_err();
    assert!(matches!(err, ConfigError::Toml(_)), "{err:?}");
}

#[test]
fn load_rejects_undefined_var_reference() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
[mcp.servers.s]
url = { var = "missing" }
"#,
    );
    let err = load_at(&path, "s").unwrap_err();
    assert!(
        matches!(&err, ConfigError::VarResolve(VarResolveError::UndefinedVar(n)) if n == "missing"),
        "{err:?}"
    );
}

#[test]
fn transport_comes_from_toml() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
[mcp.servers.s1]
url = "https://ok"
transport = "from-toml"
"#,
    );
    let resolved = load_at(&path, "s1").unwrap();
    assert_eq!(resolved.transport.as_deref(), Some("from-toml"));
}

#[test]
fn load_missing_config_yields_not_found() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("missing.toml");
    assert!(matches!(
        load_mcp_at(&path, "s1").unwrap_err(),
        ConfigError::NotFound(_)
    ));
}

#[test]
fn header_value_parses_plain_varref_and_segments() {
    #[derive(Debug, Deserialize)]
    struct H {
        headers: HashMap<String, VarTemplate>,
    }
    let t = r#"
[headers]
A = "plain"
B = { var = "tok" }
C = ["x-", { var = "tok" }, "-z"]
"#;
    let h: H = toml::from_str(t).unwrap();
    match &h.headers["A"] {
        VarTemplate::Single(Segment::Literal(s)) => assert_eq!(s, "plain"),
        _ => panic!("expected Single(Literal)"),
    }
    match &h.headers["B"] {
        VarTemplate::Single(Segment::Ref(VarRef { var })) => assert_eq!(var, "tok"),
        _ => panic!("expected Single(Ref)"),
    }
    match &h.headers["C"] {
        VarTemplate::Segments(segs) => assert_eq!(segs.len(), 3),
        _ => panic!("expected Segments"),
    }
}

#[test]
fn load_url_from_segments() {
    let host = String::from("TRG_SEG_URL_HOST");
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let cfg = format!(
        r#"
[mcp.servers.composed]
url = [
"https://",
{{ var = "host" }},
"/v1/stream",
]

[mcp.servers.composed.vars]
host = {{ env = "{host}" }}
"#,
    );
    write_secure_config(&path, &cfg);
    let r = load_at_with_env(&path, "composed", &|name| {
        (name == host).then(|| "mcp.example.org".to_string())
    })
    .unwrap();
    assert_eq!(r.url.expose_secret(), "https://mcp.example.org/v1/stream");
}

#[test]
fn load_header_from_segments() {
    let token = String::from("TRG_SEG_TOKEN");
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let cfg = format!(
        r#"
[mcp.servers.h]
url = "https://ok"

[mcp.servers.h.vars]
token = {{ env = "{token}" }}

[mcp.servers.h.headers]
Authorization = ["Bearer ", {{ var = "token" }}]
"#,
    );
    write_secure_config(&path, &cfg);
    let r = load_at_with_env(&path, "h", &|name| (name == token).then(|| "abc123".to_string())).unwrap();
    let auth = http::HeaderName::from_static("authorization");
    assert_eq!(r.http_headers[&auth].expose_secret(), "Bearer abc123");
}

#[test]
fn load_segment_propagates_missing_env() {
    let missing = String::from("TRG_SEG_REQ");
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let cfg = format!(
        r#"
[mcp.servers.s]
url = ["https://", {{ var = "host" }}, "/x"]

[mcp.servers.s.vars]
host = {{ env = "{missing}" }}
"#,
    );
    write_secure_config(&path, &cfg);
    let err = load_at_with_env(&path, "s", &|_| None).unwrap_err();
    assert!(
        matches!(&err, ConfigError::VarResolve(VarResolveError::MissingEnv(n)) if n == &missing),
        "{err:?}"
    );
}

#[test]
fn load_vars_table_supports_literal_and_env() {
    let host = String::from("TRG_VARS_HOST");
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let cfg = format!(
        r#"
[mcp.servers.both]
url = ["https://", {{ var = "host" }}, "/", {{ var = "api" }}, "/stream"]

[mcp.servers.both.vars]
host = {{ env = "{host}" }}
api  = "v1"
"#,
    );
    write_secure_config(&path, &cfg);
    let r = load_at_with_env(&path, "both", &|name| (name == host).then(|| "h.example".to_string())).unwrap();
    assert_eq!(r.url.expose_secret(), "https://h.example/v1/stream");
}

fn load_exec_full(path: &Path, name: &str) -> Result<LoadedExec, ConfigError> {
    load_exec_at(path, name)?.finish(&FetchedSecrets::new())
}

#[test]
fn a_exec_entry_carries_its_command_args_and_secrets_section() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
        [secrets.backends.work]
        kind = "openbao"
        addr = "https://bao.example.com:8200"
        mount = "secret"
        path_prefix = "trg"
        owner = "yordis"
        token = "irrelevant"

        [exec.claude]
        command = "claude"
        args = ["--dangerously-skip-permissions"]
        "#,
    );

    let loaded = load_exec_full(&path, "claude").unwrap();
    assert_eq!(loaded.command, "claude");
    assert_eq!(loaded.args, vec!["--dangerously-skip-permissions".to_string()]);
    assert!(loaded.env.is_empty());
    assert!(loaded.unset.is_empty());

    let pending = load_exec_at(&path, "claude").unwrap();
    assert!(
        pending.secrets.backends.contains_key("work"),
        "the section travels with every entry, same as it does for mcp servers"
    );
}

#[test]
fn an_unknown_exec_entry_lists_the_ones_that_are_declared() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
        [exec.zebra]
        command = "z"

        [exec.alpha]
        command = "a"
        "#,
    );

    let err = load_exec_at(&path, "missing").unwrap_err();
    let ConfigError::UnknownExecEntry { name, available } = err else {
        panic!("expected UnknownExecEntry, got {err:?}");
    };
    assert_eq!(name, "missing");
    let listed: HashSet<&str> = available.split(", ").collect();
    assert_eq!(listed, HashSet::from(["alpha", "zebra"]));
}

#[test]
fn a_config_without_an_exec_table_yields_no_exec_entries() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(&path, "");
    assert!(matches!(
        load_exec_at(&path, "x").unwrap_err(),
        ConfigError::NoExecEntries
    ));
}

#[test]
fn a_exec_entry_resolves_secret_and_literal_env_and_dedups_the_vars_it_wants() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
        [secrets.backends.homelab]
        kind = "keychain"

        [exec.p]
        command = "claude"
        unset = ["ANTHROPIC_API_KEY"]

        [exec.p.env]
        MODE = "prod"
        TOKEN = { backend = "homelab", path = "agentgateway", key = "token" }
        TOKEN_AGAIN = { backend = "homelab", path = "agentgateway", key = "token" }
        "#,
    );

    let pending = load_exec_at(&path, "p").unwrap();
    let wanted = pending.secret_vars();
    assert_eq!(wanted.len(), 1, "two vars at the same address are one read: {wanted:?}");

    let mut fetched = FetchedSecrets::new();
    fetched.insert(wanted[0].clone(), SecretString::from("t".to_string()));

    let loaded = pending.finish(&fetched).unwrap();
    assert_eq!(loaded.unset, vec!["ANTHROPIC_API_KEY".to_string()]);
    assert_eq!(loaded.env["MODE"], "prod");
    assert_eq!(loaded.env["TOKEN"], "t");
    assert_eq!(loaded.env["TOKEN_AGAIN"], "t");
}

#[test]
fn listing_exec_names_sorts_them() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        r#"
        [exec.zebra]
        command = "z"

        [exec.alpha]
        command = "a"
        "#,
    );

    assert_eq!(list_exec_names_at(&path).unwrap(), vec!["alpha", "zebra"]);
}

#[test]
fn listing_exec_names_on_a_config_without_an_exec_table_is_an_empty_list_not_an_error() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(&path, "");

    assert_eq!(list_exec_names_at(&path).unwrap(), Vec::<String>::new());
}

#[test]
fn a_exec_entry_resolves_a_composed_array_env_value() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let key = String::from("TRG_EXEC_COMPOSED");

    write_secure_config(
        &path,
        &format!(
            r#"
        [exec.p]
        command = "claude"

        [exec.p.env]
        DIR = [{{ env = "{key}" }}, "/app/state"]
        "#,
        ),
    );

    let loaded = load_exec_at(&path, "p")
        .unwrap()
        .finish_with_env(&FetchedSecrets::new(), &|name| {
            (name == key).then(|| "/home/tester".to_string())
        })
        .unwrap();
    assert_eq!(loaded.env["DIR"], "/home/tester/app/state");
}

#[test]
fn loading_is_one_span_that_names_no_path_and_no_literal() {
    const TOKEN: &str = "config-span-token-6e0b";
    let dir = tempdir().unwrap();
    let path = dir.path().join("config.toml");
    write_secure_config(
        &path,
        &format!(
            r#"
        [secrets.backends.work]
        kind = "openbao"
        addr = "https://bao.example.com:8200"
        mount = "secret"
        path_prefix = "trg"
        owner = "yordis"
        token = "{TOKEN}"

        [exec.claude]
        command = "claude"
        "#
        ),
    );

    let (loaded, trace) = crate::telemetry::testing::capture(|| traced_load(|| load_exec_at(&path, "claude")));
    loaded.expect("load");
    assert!(trace.span("load config").is_some());
    assert_eq!(trace.attribute("load config", "error.type"), None);
    assert_never_recorded(&trace, &[TOKEN, &dir.path().display().to_string()]);

    let (missing, trace) = crate::telemetry::testing::capture(|| traced_load(|| load_exec_at(&path, "nope")));
    assert!(missing.is_err());
    assert_eq!(
        trace
            .attribute("load config", "error.type")
            .map(|v| v.to_string())
            .as_deref(),
        Some("unknown_exec_entry")
    );
}
