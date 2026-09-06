use std::collections::HashMap;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

/// Where in a secrets backend one value lives.
///
/// The three coordinates are spelled out rather than packed into one string,
/// because a `<path>#<key>` form would have to be split, escaped and rejected
/// at every boundary it crosses, and because a named field is what the rest of
/// this table already looks like.
///
/// A declaration is the secret's whole identity and names nothing about who
/// reads it, so the same inline table can be pasted into as many servers as
/// need that value.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct SecretVar {
    /// The `[secrets.backends.<name>]` entry to read from, named here rather
    /// than inherited from the server, so a var says where it comes from
    /// without the reader tracing it through anything.
    pub backend: String,
    pub path: String,
    pub key: String,
}

impl SecretVar {
    /// The `trg secret put` invocation that would write this var, so an error
    /// about a missing one can hand back the fix rather than describe it.
    ///
    /// Quoted, because a path is allowed a space and a command offered as the
    /// fix has to survive being pasted.
    pub fn put_command(&self) -> String {
        let q = crate::shell::quote_for_shell;
        format!(
            "trg secret put --backend {} --path {} --key {}",
            q(&self.backend),
            q(&self.path),
            q(&self.key)
        )
    }

    /// The inline table that declares this var in a server's `vars`, so the
    /// address just written and the address that will be read cannot be
    /// spelled differently.
    ///
    /// Escaped for the same reason `put_command` is quoted: a path may hold a
    /// character that would otherwise end the TOML string early.
    pub fn declaration(&self) -> String {
        format!(
            "{{ backend = {}, path = {}, key = {} }}",
            toml_basic_string(&self.backend),
            toml_basic_string(&self.path),
            toml_basic_string(&self.key)
        )
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

impl std::fmt::Display for SecretVar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` from backend `{}` at `{}`", self.key, self.backend, self.path)
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
/// table; `Secret` is an inline `{ backend, path, key }` table.
/// `VarSource` is intentionally accepted only inside a `vars` table — never directly
/// in `url` or header values.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
#[serde(expecting = "a string, or an inline table `{ env = \"NAME\", default = \"...\" }` \
                     or `{ backend = \"NAME\", path = \"...\", key = \"...\" }`")]
pub enum VarSource {
    Literal(String),
    Env {
        env: String,
        #[serde(default)]
        default: Option<String>,
    },
    Secret(SecretVar),
}

impl VarSource {
    /// The backend read this source needs before it can resolve, if any.
    pub fn secret(&self) -> Option<&SecretVar> {
        match self {
            VarSource::Secret(v) => Some(v),
            VarSource::Literal(_) | VarSource::Env { .. } => None,
        }
    }

    pub fn resolve(&self, fetched: &FetchedSecrets) -> Result<String, VarResolveError> {
        match self {
            VarSource::Literal(s) => Ok(s.clone()),
            VarSource::Env { env, default } => match std::env::var(env) {
                Ok(v) => Ok(v),
                Err(_) => default.clone().ok_or_else(|| VarResolveError::MissingEnv(env.clone())),
            },
            // Absent here means the caller resolved without fetching first,
            // which is a wiring mistake rather than anything the config said.
            VarSource::Secret(v) => fetched
                .get(v)
                .map(|s| s.expose_secret().to_string())
                .ok_or_else(|| VarResolveError::SecretNotFetched(v.clone())),
        }
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
}

/// A value for `[exec.<name>.env]`: either one `VarSource`, or an array of
/// them concatenated in order.
///
/// Unlike `VarTemplate` (used for `url`/headers), each array element may be a
/// full `VarSource` — including `{ env = ... }` and
/// `{ backend = ..., path = ..., key = ... }` — because an exec entry's `env`
/// table has no separate `vars` table to route an indirect reference
/// through; it is already the one place these bindings live.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum EnvValue {
    Scalar(VarSource),
    Composed(Vec<VarSource>),
}

impl EnvValue {
    /// Every backend read this value needs before it can resolve.
    pub fn secrets(&self) -> Vec<&SecretVar> {
        match self {
            EnvValue::Scalar(v) => v.secret().into_iter().collect(),
            EnvValue::Composed(segs) => segs.iter().filter_map(|s| s.secret()).collect(),
        }
    }

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
        let mut m: HashMap<String, VarSource> = toml::from_str(
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
        let m: HashMap<String, VarSource> =
            toml::from_str(r#"t = { backend = "homelab", path = "agentgateway", key = "token" }"#).unwrap();
        let want = SecretVar {
            backend: "homelab".into(),
            path: "agentgateway".into(),
            key: "token".into(),
        };
        assert_eq!(m["t"].secret(), Some(&want));
    }

    /// The address is the secret's whole identity, so two declarations of the
    /// same secret are the same key no matter which server wrote them.
    #[test]
    fn two_declarations_of_one_address_are_one_key() {
        let m: HashMap<String, VarSource> = toml::from_str(
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
        let var = SecretVar {
            backend: "homelab".into(),
            path: "agentgateway".into(),
            key: "token".into(),
        };
        let mut fetched = FetchedSecrets::new();
        fetched.insert(var.clone(), SecretString::from("t".to_string()));

        assert_eq!(VarSource::Secret(var).resolve(&fetched).unwrap(), "t");
    }

    /// Resolving before fetching is the caller's mistake, and the message has
    /// to say so rather than blaming the config for a value it declared fine.
    #[test]
    fn varsource_secret_without_a_fetch_is_reported_as_such() {
        let var = SecretVar {
            backend: "homelab".into(),
            path: "agentgateway".into(),
            key: "token".into(),
        };
        let err = VarSource::Secret(var).resolve(&FetchedSecrets::new()).unwrap_err();
        assert!(matches!(err, VarResolveError::SecretNotFetched(_)), "{err}");
    }

    #[test]
    fn varsource_rejects_a_half_spelled_secret() {
        let err = toml::from_str::<HashMap<String, VarSource>>(r#"t = { backend = "homelab", path = "agentgateway" }"#)
            .unwrap_err();
        assert!(format!("{err}").contains("backend"), "{err}");
    }

    #[test]
    fn varsource_rejects_unknown_field() {
        let err = toml::from_str::<VarSource>(r#"{ env = "E", typo = true }"#).unwrap_err();
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

    #[test]
    fn envvalue_composed_literal_and_env_segments_concatenate() {
        let name = format!("TRG_ENVVALUE_ENV_{}", std::process::id());
        std::env::set_var(&name, "/home/tester");
        #[derive(Deserialize)]
        struct W {
            v: EnvValue,
        }
        let w: W = toml::from_str(&format!(r#"v = [{{ env = "{name}" }}, "/app/state"]"#)).unwrap();
        assert_eq!(w.v.resolve(&FetchedSecrets::new()).unwrap(), "/home/tester/app/state");
        std::env::remove_var(&name);
    }

    #[test]
    fn envvalue_composed_secret_segment_is_collected_and_resolved() {
        #[derive(Deserialize)]
        struct W {
            v: EnvValue,
        }
        let w: W =
            toml::from_str(r#"v = ["prefix-", { backend = "onepassword", path = "Ops/deploy-keys", key = "TOKEN" }]"#)
                .unwrap();
        let want = SecretVar {
            backend: "onepassword".into(),
            path: "Ops/deploy-keys".into(),
            key: "TOKEN".into(),
        };
        assert_eq!(w.v.secrets(), vec![&want]);

        let mut fetched = FetchedSecrets::new();
        fetched.insert(want, SecretString::from("secretvalue".to_string()));
        assert_eq!(w.v.resolve(&fetched).unwrap(), "prefix-secretvalue");
    }

    #[test]
    fn envvalue_empty_composed_resolves_to_empty_string() {
        #[derive(Deserialize)]
        struct W {
            v: EnvValue,
        }
        let w: W = toml::from_str("v = []").unwrap();
        assert_eq!(w.v.resolve(&FetchedSecrets::new()).unwrap(), "");
    }

    #[test]
    fn envvalue_scalar_behaves_like_a_bare_varsource() {
        #[derive(Deserialize)]
        struct W {
            v: EnvValue,
        }

        let w: W = toml::from_str(r#"v = "plain""#).unwrap();
        assert!(w.v.secrets().is_empty());
        assert_eq!(w.v.resolve(&FetchedSecrets::new()).unwrap(), "plain");

        let name = format!("TRG_ENVVALUE_SCALAR_ENV_{}", std::process::id());
        std::env::set_var(&name, "hi");
        let w: W = toml::from_str(&format!(r#"v = {{ env = "{name}" }}"#)).unwrap();
        assert_eq!(w.v.resolve(&FetchedSecrets::new()).unwrap(), "hi");
        std::env::remove_var(&name);

        let w: W =
            toml::from_str(r#"v = { backend = "onepassword", path = "Ops/deploy-keys", key = "TOKEN" }"#).unwrap();
        let want = SecretVar {
            backend: "onepassword".into(),
            path: "Ops/deploy-keys".into(),
            key: "TOKEN".into(),
        };
        assert_eq!(w.v.secrets(), vec![&want]);
        let mut fetched = FetchedSecrets::new();
        fetched.insert(want, SecretString::from("s".to_string()));
        assert_eq!(w.v.resolve(&fetched).unwrap(), "s");
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

    /// A path may hold a space, so a command offered as the fix has to survive
    /// being pasted into a shell.
    #[test]
    fn a_put_command_quotes_what_a_shell_would_otherwise_split() {
        let var = SecretVar {
            backend: "home lab".to_string(),
            path: "mcp/a b".to_string(),
            key: "token".to_string(),
        };
        assert_eq!(
            var.put_command(),
            "trg secret put --backend 'home lab' --path 'mcp/a b' --key token"
        );
    }

    #[test]
    fn a_plain_put_command_is_left_unquoted() {
        let var = SecretVar {
            backend: "homelab".to_string(),
            path: "mcp/memorizer".to_string(),
            key: "token".to_string(),
        };
        assert_eq!(
            var.put_command(),
            "trg secret put --backend homelab --path mcp/memorizer --key token"
        );
    }

    /// The declaration is meant to be pasted back into `vars`, so it has to
    /// parse as the table it claims to be.
    #[test]
    fn a_declaration_survives_a_quote_in_a_path() {
        let var = SecretVar {
            backend: "homelab".to_string(),
            path: r#"mcp/a"b\c"#.to_string(),
            key: "token".to_string(),
        };
        let toml_text = format!("[vars]\ntoken = {}\n", var.declaration());
        let parsed: toml::Value = toml::from_str(&toml_text).expect("declaration must parse");
        assert_eq!(parsed["vars"]["token"]["path"].as_str().unwrap(), r#"mcp/a"b\c"#);
    }

    #[test]
    fn a_declaration_keeps_a_newline_on_one_line() {
        let var = SecretVar {
            backend: "homelab".to_string(),
            path: "a\nb".to_string(),
            key: "token".to_string(),
        };
        let line = var.declaration();
        assert!(!line.contains('\n'), "{line}");
        let parsed: toml::Value = toml::from_str(&format!("[vars]\ntoken = {line}\n")).expect("parse");
        assert_eq!(parsed["vars"]["token"]["path"].as_str().unwrap(), "a\nb");
    }
}
