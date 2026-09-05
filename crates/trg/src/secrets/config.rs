//! `[secrets]` deserialisation and resolution into a [`Registry`].
//!
//! Backends are **addressed, never searched**. A server names exactly one
//! backend and that is the one used; there is no ordered list, no probing, and
//! no fallback to a second backend when the first fails.
//!
//! Backend declarations resolve through [`VarSource`] only, so a backend can
//! read `{ env = "BAO_ADDR" }` but cannot read `{ secret = "..." }`. That keeps
//! bootstrap acyclic: nothing needed to reach a secret store may itself live in
//! a secret store.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

use super::keychain::{KeychainBackend, DEFAULT_SERVICE};
use super::openbao::{
    expand_tilde, OpenBaoBackend, OpenBaoBuildError, OpenBaoSettings, TokenSource, DEFAULT_TIMEOUT_MS,
};
use super::Backend;
use crate::config::{VarResolveError, VarSource};

/// The `[secrets]` table.
#[derive(Debug, Default, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct SecretsSection {
    #[serde(default)]
    pub backends: HashMap<String, BackendConfig>,
}

/// One `[secrets.backends.<name>]` entry.
///
/// `kind` is a closed enum, so an unrecognised value fails at parse time with
/// serde naming the kinds that do exist.
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackendConfig {
    Keychain(KeychainConfig),
    Openbao(Box<OpenbaoConfig>),
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct KeychainConfig {
    #[serde(default)]
    pub service: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct OpenbaoConfig {
    pub addr: VarSource,
    pub mount: String,
    pub path_prefix: String,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub machine_id: Option<String>,
    #[serde(default)]
    pub token_file: Option<String>,
    #[serde(default)]
    pub token: Option<VarSource>,
    #[serde(default)]
    pub ca_cert_file: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

impl BackendConfig {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Keychain(_) => "keychain",
            Self::Openbao(_) => "openbao",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("`{name}` is not a declared secrets backend; declared: {declared}")]
    Unknown { name: String, declared: String },

    #[error("`{name}` is not a declared secrets backend; add a `[secrets.backends.{name}]` section")]
    NoneDeclared { name: String },

    #[error("`[secrets.backends.{name}]`: declare exactly one of `token_file` or `token`, not {found}")]
    TokenSource { name: String, found: &'static str },

    #[error("`[secrets.backends.{name}]`: `{field}` could not be resolved: {cause}")]
    Resolve {
        name: String,
        field: &'static str,
        #[source]
        cause: VarResolveError,
    },

    #[error("`[secrets.backends.{name}]`: {cause}")]
    Build {
        name: String,
        #[source]
        cause: OpenBaoBuildError,
    },
}

/// The declared backends, resolved by name on demand.
///
/// Resolution is lazy on purpose: a machine that has `BAO_ADDR` unset should
/// still be able to use an unrelated keychain-backed server, so declaring a
/// backend costs nothing until something addresses it. It is still built once
/// per process, never per operation.
#[derive(Debug, Default, Clone)]
pub struct Registry {
    declared: HashMap<String, BackendConfig>,
}

impl Registry {
    pub fn new(section: SecretsSection) -> Self {
        Self {
            declared: section.backends,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.declared.is_empty()
    }

    /// Declared backend names, sorted.
    pub fn declared(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.declared.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// Declared backend names, sorted, for error messages.
    pub fn names(&self) -> String {
        self.declared().join(", ")
    }

    pub fn resolve(&self, name: &str) -> Result<Backend, BackendError> {
        let Some(config) = self.declared.get(name) else {
            if self.declared.is_empty() {
                return Err(BackendError::NoneDeclared { name: name.to_string() });
            }
            return Err(BackendError::Unknown {
                name: name.to_string(),
                declared: self.names(),
            });
        };
        build(name, config)
    }

    /// The backend a server gets when it names none.
    ///
    /// This is what every macOS user had before `[secrets]` existed, so an
    /// existing config keeps working untouched and existing keychain items stay
    /// addressable. It is a default, not a fallback: nothing falls back to it
    /// when a named backend fails.
    pub fn default_backend() -> Backend {
        Backend::Keychain(KeychainBackend::with_default_service())
    }

    /// The backend one MCP server addresses.
    pub fn for_server(&self, server: &str, declared: Option<&str>) -> Result<Backend, ServerBackendError> {
        match declared {
            None => Ok(Self::default_backend()),
            Some(name) => self.resolve(name).map_err(|cause| ServerBackendError {
                server: server.to_string(),
                backend: name.to_string(),
                cause: Box::new(cause),
            }),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("`[mcp.servers.{server}]` names `secrets = \"{backend}\"`, but {cause}")]
pub struct ServerBackendError {
    pub server: String,
    pub backend: String,
    #[source]
    pub cause: Box<BackendError>,
}

fn build(name: &str, config: &BackendConfig) -> Result<Backend, BackendError> {
    match config {
        BackendConfig::Keychain(KeychainConfig { service }) => Ok(Backend::Keychain(KeychainBackend::new(
            service.clone().unwrap_or_else(|| DEFAULT_SERVICE.to_string()),
        ))),
        BackendConfig::Openbao(openbao) => {
            let OpenbaoConfig {
                addr,
                mount,
                path_prefix,
                owner,
                machine_id,
                token_file,
                token,
                ca_cert_file,
                timeout_ms,
            } = openbao.as_ref();
            let addr = addr.resolve().map_err(|cause| BackendError::Resolve {
                name: name.to_string(),
                field: "addr",
                cause,
            })?;

            let token = match (token_file, token) {
                (Some(file), None) => TokenSource::File(expand_tilde(file)),
                (None, Some(var)) => TokenSource::Var(var.clone()),
                (None, None) => {
                    return Err(BackendError::TokenSource {
                        name: name.to_string(),
                        found: "neither",
                    })
                }
                (Some(_), Some(_)) => {
                    return Err(BackendError::TokenSource {
                        name: name.to_string(),
                        found: "both",
                    })
                }
            };

            // Two separate axes, and neither is derived from anything.
            //
            // `owner` says whose credential it is, which is the boundary an
            // ACL draws. `machine_id` says which holder may refresh it, which
            // matters only where a provider rotates refresh tokens and treats
            // a reused one as replay. One person on two machines is one owner
            // and two holders, so neither substitutes for the other.
            let settings = OpenBaoSettings {
                addr,
                mount: mount.clone(),
                path_prefix: path_prefix.clone(),
                owner: owner.clone(),
                machine_id: machine_id.clone(),
                token,
                ca_cert_file: ca_cert_file.as_deref().map(expand_tilde),
                timeout: Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)),
            };

            OpenBaoBackend::new(settings)
                .map(Backend::OpenBao)
                .map_err(|cause| BackendError::Build {
                    name: name.to_string(),
                    cause,
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(toml_text: &str) -> Result<SecretsSection, toml::de::Error> {
        toml::from_str(toml_text)
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
        assert_eq!(s.backends["local"].kind(), "keychain");
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
            token_file = "~/.vault-token"
            "#,
        )
        .expect("parse");
        assert_eq!(s.backends["work"].kind(), "openbao");
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
    fn an_owner_and_a_machine_id_are_both_optional_and_neither_is_derived() {
        let s = section(
            r#"
            [backends.work]
            kind = "openbao"
            addr = "https://bao:8200"
            mount = "secret"
            path_prefix = "trg"
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
}
