//! The process environment a single eval run is executed in.
//!
//! A run inherits nothing by accident. Everything the harness subprocess can see is
//! either on an explicit allowlist or was put there for this run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::forwarding::{is_exporter_header_var, CodexOtelConfig, HarnessPassthrough, TelemetryForwarding};
use super::Runner;
use crate::agentskills::case_env::CaseEnv;
use crate::agentskills::redact::is_secret_env_key;
use crate::agentskills::report::EnvironmentPolicy;
use crate::telemetry::ContentCapture;

mod constants;

pub use constants::RUN_HOME_DIR_NAME;
use constants::{CLOUD_CREDENTIAL_PREFIXES, NETWORK_VARS, PROCESS_VARS};

/// Where a harness keeps the per-user state that changes what its agent does: installed
/// skills, global instruction files, MCP servers, permission settings, and history.
///
/// This is the state that makes an eval unreproducible on someone else's machine, so
/// [`EnvironmentPolicy::Isolated`] moves it aside for the duration of a run.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum HarnessConfigHome {
    /// The harness honours a variable, so a run's config home can be named directly.
    Redirectable { var: &'static str, dir_name: &'static str },
    /// The harness has no override, so a run's config home only moves when `HOME` does.
    HomeRelative { dir_name: &'static str },
}

impl HarnessConfigHome {
    fn dir_name(self) -> &'static str {
        match self {
            Self::Redirectable { dir_name, .. } | Self::HomeRelative { dir_name } => dir_name,
        }
    }

    fn resolve_on_host(self, host: &BTreeMap<String, String>) -> Option<PathBuf> {
        if let Self::Redirectable { var, .. } = self {
            if let Some(configured) = host.get(var) {
                return Some(PathBuf::from(configured));
            }
        }
        host.get("HOME").map(|home| Path::new(home).join(self.dir_name()))
    }

    /// How this config home is found, independent of where it currently points.
    fn basis(self) -> ConfigHomeBasis {
        match self {
            Self::Redirectable { var, .. } => ConfigHomeBasis::Variable { name: var.to_string() },
            Self::HomeRelative { .. } => ConfigHomeBasis::HomeRelative,
        }
    }
}

/// Whether a run's config home belonged to the operator or was made for the run.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConfigHomeOrigin {
    /// The run saw the operator's own config home, with its skills, instructions, MCP
    /// servers, and history.
    Host,
    /// The config home was created for this run and holds only what an isolated run
    /// needs to authenticate.
    Run,
}

/// How a harness's config home was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigHomeBasis {
    /// The harness honours this environment variable, so the config home was named
    /// directly rather than derived.
    Variable { name: String },
    /// The harness has no override; the config home sits at a fixed name under `HOME`.
    HomeRelative,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct DeclaredConfigHome {
    path: String,
    origin: ConfigHomeOrigin,
    basis: ConfigHomeBasis,
}

/// Where a run's harness found its config home, recorded for `env.json`.
///
/// A bare path cannot say whether a run inherited the operator's skills, instructions,
/// and history or was handed a config home made for it, and it cannot say whether the
/// same run on another machine lands in the same place. [`ConfigHomeOrigin`] answers the
/// first, [`ConfigHomeBasis`] the second, which is why this is a type rather than a
/// string a reader has to interpret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedConfigHome {
    path: PathBuf,
    origin: ConfigHomeOrigin,
    basis: ConfigHomeBasis,
}

impl RecordedConfigHome {
    /// `--out-dir` is routinely relative, and a run directory derived from it is relative
    /// in turn, so a config home built underneath it needs resolving rather than
    /// rejecting: a reader of `env.json` cannot resolve a relative path themselves once
    /// the run is over and the working directory that made it meaningful is gone.
    /// [`std::path::absolute`] resolves against the current working directory and
    /// normalizes lexically, so this fails only when that directory cannot be read.
    pub fn new(path: PathBuf, origin: ConfigHomeOrigin, basis: ConfigHomeBasis) -> std::io::Result<Self> {
        let path = std::path::absolute(path)?;
        Ok(Self { path, origin, basis })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn origin(&self) -> ConfigHomeOrigin {
        self.origin
    }

    pub fn basis(&self) -> &ConfigHomeBasis {
        &self.basis
    }
}

impl Serialize for RecordedConfigHome {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        DeclaredConfigHome {
            path: path_string(&self.path),
            origin: self.origin,
            basis: self.basis.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RecordedConfigHome {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let declared = DeclaredConfigHome::deserialize(deserializer)?;
        Self::new(PathBuf::from(declared.path), declared.origin, declared.basis).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for RecordedConfigHome {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        DeclaredConfigHome::schema_name()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        DeclaredConfigHome::json_schema(generator)
    }
}

/// The document written to a run's `env.json`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RecordedEnvironment {
    pub vars: BTreeMap<String, String>,
    /// Absent only when the host gave a run neither the harness's override variable nor
    /// a `HOME` to resolve a config home against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_home: Option<RecordedConfigHome>,
}

impl Runner {
    pub fn config_home(self) -> HarnessConfigHome {
        match self {
            Self::ClaudeCode => HarnessConfigHome::Redirectable {
                var: "CLAUDE_CONFIG_DIR",
                dir_name: ".claude",
            },
            Self::Codex => HarnessConfigHome::Redirectable {
                var: "CODEX_HOME",
                dir_name: ".codex",
            },
            Self::CursorAgent => HarnessConfigHome::HomeRelative { dir_name: ".cursor" },
        }
    }

    /// The variables a run needs to authenticate against this harness's model provider.
    ///
    /// Deliberately an explicit list rather than a vendor prefix. Running `trg` from
    /// inside an agent session puts that session's own identity in the environment,
    /// including a live IPC socket and token, and a prefix match would hand every run a
    /// channel back into the session that launched it.
    fn credential_vars(self) -> &'static [&'static str] {
        match self {
            Self::ClaudeCode => &[
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "ANTHROPIC_BASE_URL",
                "ANTHROPIC_CUSTOM_HEADERS",
                "CLAUDE_CODE_OAUTH_TOKEN",
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
            ],
            Self::Codex => &[
                "CODEX_API_KEY",
                "CODEX_ACCESS_TOKEN",
                "OPENAI_API_KEY",
                "OPENAI_BASE_URL",
            ],
            Self::CursorAgent => &["CURSOR_API_KEY"],
        }
    }

    /// A variable that tells the harness where to look up credentials, independent of
    /// where its config home is.
    ///
    /// Some harnesses derive the identity of their credential store from the config home
    /// path, so redirecting the config home invalidates a working login. Where such a
    /// variable exists, an isolated run pins it to the host config home so the login
    /// survives the redirect.
    fn credential_locator_var(self) -> Option<&'static str> {
        match self {
            Self::ClaudeCode => Some("CLAUDE_SECURESTORAGE_CONFIG_DIR"),
            Self::Codex | Self::CursorAgent => None,
        }
    }

    /// Entries inside the config home that an isolated run is given, so it can
    /// authenticate without also inheriting the skills, instructions, and MCP servers
    /// sitting beside them.
    fn auth_entries(self) -> &'static [AuthEntry] {
        match self {
            Self::ClaudeCode => &[AuthEntry::Whole(".credentials.json")],
            Self::Codex => &[AuthEntry::Whole("auth.json")],
            Self::CursorAgent => &[AuthEntry::Reduced {
                file: "cli-config.json",
                keep: &["authInfo"],
            }],
        }
    }
}

/// How one entry of a harness config home is carried into an isolated run.
///
/// A file that is credentials and nothing else can be handed over whole. A harness that
/// keeps its login in the same file as its settings cannot: permissions, approval mode,
/// and model selection live there too, and those are exactly what an isolated run is
/// supposed to hold still, so only the members that carry the login are copied across.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum AuthEntry {
    Whole(&'static str),
    Reduced {
        file: &'static str,
        keep: &'static [&'static str],
    },
}

impl AuthEntry {
    fn file(self) -> &'static str {
        match self {
            Self::Whole(file) | Self::Reduced { file, .. } => file,
        }
    }

    /// Anything unrecognized is left behind rather than carried across, so a harness that
    /// moves its login somewhere else fails to authenticate instead of quietly handing the
    /// run the operator's settings again.
    fn carry(self, source: &Path, destination: &Path) -> std::io::Result<()> {
        match self {
            Self::Whole(_) => std::os::unix::fs::symlink(source, destination),
            Self::Reduced { keep, .. } => {
                let Ok(serde_json::Value::Object(members)) =
                    serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(source)?)
                else {
                    return Ok(());
                };
                let kept: serde_json::Map<String, serde_json::Value> = keep
                    .iter()
                    .filter_map(|name| members.get_key_value(*name))
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect();
                std::fs::write(destination, serde_json::Value::Object(kept).to_string())
            }
        }
    }
}

/// The environment one run's harness subprocess is given.
#[derive(Debug, Clone)]
pub struct RunEnvironment {
    policy: EnvironmentPolicy,
    vars: BTreeMap<String, String>,
    /// Held apart from `vars` because they are the one thing every policy hands over:
    /// under `Inherited` nothing is cleared, so there is no assembled environment to fold
    /// them into, and a case that declared them would otherwise see them only when the
    /// operator happened not to be inheriting.
    case_vars: BTreeMap<String, String>,
    config_home: Option<RecordedConfigHome>,
    /// Held apart because, unlike `vars`, they reach the harness under `Inherited` too.
    telemetry_vars: BTreeMap<String, String>,
    forwards_traces: bool,
    codex_otel: Option<CodexOtelConfig>,
}

impl RunEnvironment {
    pub fn prepare(
        runner: Runner,
        run_dir: &Path,
        policy: EnvironmentPolicy,
        case_env: Option<&CaseEnv>,
        forwarding: TelemetryForwarding,
    ) -> std::io::Result<Self> {
        let host = host_environment();
        let mut environment = Self::prepare_from(runner, run_dir, policy, case_env, &host)?;
        environment.forward_telemetry(runner, forwarding, &host, ContentCapture::from_env());
        Ok(environment)
    }

    pub(crate) fn forward_telemetry(
        &mut self,
        runner: Runner,
        forwarding: TelemetryForwarding,
        host: &BTreeMap<String, String>,
        content: ContentCapture,
    ) {
        self.telemetry_vars = forwarding.harness_vars(runner, self.policy, host, content);
        self.forwards_traces = forwarding.forwards_traces(runner, host);
        self.codex_otel = (matches!(forwarding, TelemetryForwarding::On)
            && runner.telemetry_passthrough() == HarnessPassthrough::CodexConfig)
            .then(|| CodexOtelConfig::render(host, content))
            .flatten();
    }

    /// The `[otel]` table to hand a codex run, when the operator forwarded telemetry
    /// and configured somewhere to send it.
    pub fn codex_otel(&self) -> Option<&CodexOtelConfig> {
        self.codex_otel.as_ref()
    }

    /// Whether a harness configured through its environment was handed somewhere to
    /// export spans of its own, which makes rebuilding them from its stdout a duplicate.
    pub fn harness_exports_traces(&self) -> bool {
        self.forwards_traces
    }

    pub(crate) fn prepare_from(
        runner: Runner,
        run_dir: &Path,
        policy: EnvironmentPolicy,
        case_env: Option<&CaseEnv>,
        host: &BTreeMap<String, String>,
    ) -> std::io::Result<Self> {
        let basis = runner.config_home().basis();
        let case_vars: BTreeMap<String, String> = case_env
            .into_iter()
            .flat_map(CaseEnv::iter)
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();

        if matches!(policy, EnvironmentPolicy::Inherited) {
            let config_home = host_config_home_record(runner, host, basis)?;
            let mut vars = host.clone();
            vars.extend(case_vars.clone());
            return Ok(Self {
                policy,
                vars,
                case_vars,
                config_home,
                telemetry_vars: BTreeMap::new(),
                forwards_traces: false,
                codex_otel: None,
            });
        }

        let mut vars = allowlisted(runner, host);

        let config_home = if matches!(policy, EnvironmentPolicy::Isolated) {
            let host_config_home = runner.config_home().resolve_on_host(host);

            let home = run_dir.join(RUN_HOME_DIR_NAME);
            std::fs::create_dir_all(&home)?;
            vars.insert("HOME".to_string(), path_string(&home));

            let run_config_home = home.join(runner.config_home().dir_name());
            std::fs::create_dir_all(&run_config_home)?;
            if let HarnessConfigHome::Redirectable { var, .. } = runner.config_home() {
                vars.insert(var.to_string(), path_string(&run_config_home));
            }

            if let Some(host_config_home) = &host_config_home {
                link_auth_entries(runner, host_config_home, &run_config_home)?;
                if let Some(var) = runner.credential_locator_var() {
                    vars.entry(var.to_string())
                        .or_insert_with(|| path_string(host_config_home));
                }
            }

            Some(RecordedConfigHome::new(run_config_home, ConfigHomeOrigin::Run, basis)?)
        } else {
            host_config_home_record(runner, host, basis)?
        };

        vars.extend(case_vars.clone());

        Ok(Self {
            policy,
            vars,
            case_vars,
            config_home,
            telemetry_vars: BTreeMap::new(),
            forwards_traces: false,
            codex_otel: None,
        })
    }

    pub fn policy(&self) -> EnvironmentPolicy {
        self.policy
    }

    /// Where this run's harness found its config home, when the host gave it a `HOME`
    /// to resolve one against.
    pub fn config_home(&self) -> Option<&RecordedConfigHome> {
        self.config_home.as_ref()
    }

    /// The document to write to this run's `env.json`.
    pub fn record(&self) -> RecordedEnvironment {
        RecordedEnvironment {
            vars: self.recorded_vars(),
            config_home: self.config_home.clone(),
        }
    }

    pub fn apply(&self, command: &mut Command) {
        if !matches!(self.policy, EnvironmentPolicy::Inherited) {
            command.env_clear();
            command.envs(&self.vars);
        }
        command.envs(&self.telemetry_vars);
        command.envs(&self.case_vars);
    }

    /// What the run saw, for `env.json`, with secret-looking values left out.
    pub fn recorded_vars(&self) -> BTreeMap<String, String> {
        self.vars
            .iter()
            .chain(&self.telemetry_vars)
            .filter(|(key, _)| !is_secret_env_key(key) && !is_exporter_header_var(key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}

fn host_environment() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

fn host_config_home_record(
    runner: Runner,
    host: &BTreeMap<String, String>,
    basis: ConfigHomeBasis,
) -> std::io::Result<Option<RecordedConfigHome>> {
    let Some(path) = runner.config_home().resolve_on_host(host) else {
        return Ok(None);
    };
    RecordedConfigHome::new(path, ConfigHomeOrigin::Host, basis).map(Some)
}

/// `Scrubbed` leaves the harness config home alone, so the variable that names it is on
/// the allowlist. Dropping it would not leave the config home alone: the harness would
/// fall back to the default under `HOME`, which is neither the operator's config home nor
/// a config home this run set up. `Isolated` overwrites the variable afterwards.
fn allowlisted(runner: Runner, host: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let locator = runner.credential_locator_var();
    let config_home_var = match runner.config_home() {
        HarnessConfigHome::Redirectable { var, .. } => Some(var),
        HarnessConfigHome::HomeRelative { .. } => None,
    };
    let named = PROCESS_VARS
        .iter()
        .chain(NETWORK_VARS.iter())
        .chain(runner.credential_vars().iter())
        .chain(locator.iter())
        .chain(config_home_var.iter());

    let mut vars: BTreeMap<String, String> = named
        .filter_map(|name| host.get_key_value(*name))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();

    vars.extend(
        host.iter()
            .filter(|(key, _)| CLOUD_CREDENTIAL_PREFIXES.iter().any(|prefix| key.starts_with(prefix)))
            .map(|(key, value)| (key.clone(), value.clone())),
    );

    vars
}

fn link_auth_entries(runner: Runner, host_config_home: &Path, config_home: &Path) -> std::io::Result<()> {
    for entry in runner.auth_entries() {
        let source = host_config_home.join(entry.file());
        if !source.exists() {
            continue;
        }
        let destination = config_home.join(entry.file());
        if destination.symlink_metadata().is_ok() {
            continue;
        }
        entry.carry(&source, &destination)?;
    }
    Ok(())
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
#[path = "environment_tests.rs"]
mod tests;
