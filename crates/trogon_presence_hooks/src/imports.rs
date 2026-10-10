use std::fmt;

use wasmtime::component::Component;
use wasmtime::Engine;

const BASELINE: [&str; 15] = [
    "trogon:presence/types@0.1.0",
    "wasi:io/poll@0.2.12",
    "wasi:io/error@0.2.12",
    "wasi:io/streams@0.2.12",
    "wasi:clocks/monotonic-clock@0.2.12",
    "wasi:cli/stdout@0.2.12",
    "wasi:cli/stderr@0.2.12",
    "wasi:cli/stdin@0.2.12",
    "wasi:cli/environment@0.2.12",
    "wasi:cli/exit@0.2.12",
    "wasi:cli/terminal-input@0.2.12",
    "wasi:cli/terminal-output@0.2.12",
    "wasi:cli/terminal-stdin@0.2.12",
    "wasi:cli/terminal-stdout@0.2.12",
    "wasi:cli/terminal-stderr@0.2.12",
];

const HTTP: [&str; 2] = ["wasi:http/types@0.2.12", "wasi:http/outgoing-handler@0.2.12"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HttpCapability {
    Disabled,
    Enabled,
}

impl fmt::Display for HttpCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Disabled => "disabled",
            Self::Enabled => "enabled",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImportName(String);

impl ImportName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ImportName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("hook component imports {0}, which is outside the allowlist with HTTP {1}")]
pub struct UnsupportedImport(ImportName, HttpCapability);

impl UnsupportedImport {
    pub fn name(&self) -> &ImportName {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ImportAllowlist(HttpCapability);

impl ImportAllowlist {
    pub(crate) fn new(http: HttpCapability) -> Self {
        Self(http)
    }

    fn permits(self, name: &str) -> bool {
        BASELINE.contains(&name) || (self.0 == HttpCapability::Enabled && HTTP.contains(&name))
    }

    pub(crate) fn check(self, component: &Component, engine: &Engine) -> Result<(), UnsupportedImport> {
        match component
            .component_type()
            .imports(engine)
            .find(|(name, _)| !self.permits(name))
        {
            Some((name, _)) => Err(UnsupportedImport(ImportName(name.to_owned()), self.0)),
            None => Ok(()),
        }
    }
}
