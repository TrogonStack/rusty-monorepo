use std::collections::HashMap;

use serde::Deserialize;

/// A value source for `[mcp.servers.<name>.vars]` entries.
///
/// `Literal` is a bare TOML string; `Env` is an inline `{ env, default? }` table.
/// `VarSource` is intentionally accepted only inside a `vars` table — never directly
/// in `url` or header values.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
#[serde(expecting = "a string, or an inline table `{ env = \"NAME\", default = \"...\" }`")]
pub enum VarSource {
    Literal(String),
    Env {
        env: String,
        #[serde(default)]
        default: Option<String>,
    },
}

impl VarSource {
    pub fn resolve(&self) -> Result<String, VarResolveError> {
        match self {
            VarSource::Literal(s) => Ok(s.clone()),
            VarSource::Env { env, default } => match std::env::var(env) {
                Ok(v) => Ok(v),
                Err(_) => default.clone().ok_or_else(|| VarResolveError::MissingEnv(env.clone())),
            },
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VarResolveError {
    #[error("environment variable `{0}` is required but unset")]
    MissingEnv(String),

    #[error("undefined variable `{0}` referenced; declare it in `[mcp.servers.<name>.vars]`")]
    UndefinedVar(String),
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
        assert_eq!(VarSource::Literal("x".into()).resolve().unwrap(), "x");
    }

    #[test]
    fn varsource_env_resolves() {
        let name = format!("TRG_VARSRC_ENV_{}", std::process::id());
        std::env::set_var(&name, "hi");
        let e = VarSource::Env {
            env: name.clone(),
            default: None,
        };
        assert_eq!(e.resolve().unwrap(), "hi");
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
        assert_eq!(e.resolve().unwrap(), "d");
    }

    #[test]
    fn varsource_env_missing_without_default_errors() {
        let name = format!("TRG_VARSRC_REQ_{}", std::process::id());
        std::env::remove_var(&name);
        let e = VarSource::Env {
            env: name.clone(),
            default: None,
        };
        let err = e.resolve().unwrap_err();
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
}
