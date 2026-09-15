//! The process environment a single eval run is executed in.
//!
//! A run inherits nothing by accident. Everything the harness subprocess can see is
//! either on an explicit allowlist or was put there for this run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Runner;
use crate::agentskills::case_env::CaseEnv;
use crate::agentskills::redact::is_secret_env_key;
use crate::agentskills::report::EnvironmentPolicy;

/// Directory, relative to a run directory, used as `HOME` under
/// [`EnvironmentPolicy::Isolated`].
pub const RUN_HOME_DIR_NAME: &str = "home";

/// Variables without which a command line tool cannot be expected to start.
const PROCESS_VARS: &[&str] = &[
    "PATH", "HOME", "TMPDIR", "SHELL", "USER", "LOGNAME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "TZ",
];

/// Variables that decide whether the harness can reach its model provider over the
/// network at all: proxies, and the trust stores that proxies are usually paired with.
const NETWORK_VARS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "REQUESTS_CA_BUNDLE",
];

/// Prefixes for the cloud credentials a harness needs when it is pointed at a model
/// hosted on a cloud provider rather than at its vendor's own endpoint.
const CLOUD_CREDENTIAL_PREFIXES: &[&str] = &["AWS_", "GOOGLE_", "GCLOUD_", "AZURE_"];

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
}

impl RunEnvironment {
    pub fn prepare(
        runner: Runner,
        run_dir: &Path,
        policy: EnvironmentPolicy,
        case_env: Option<&CaseEnv>,
    ) -> std::io::Result<Self> {
        Self::prepare_from(runner, run_dir, policy, case_env, &host_environment())
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
        if matches!(self.policy, EnvironmentPolicy::Inherited) {
            command.envs(&self.case_vars);
            return;
        }
        command.env_clear();
        command.envs(&self.vars);
    }

    /// What the run saw, for `env.json`, with secret-looking values left out.
    pub fn recorded_vars(&self) -> BTreeMap<String, String> {
        self.vars
            .iter()
            .filter(|(key, _)| !is_secret_env_key(key))
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
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn host() -> BTreeMap<String, String> {
        [
            ("PATH", "/usr/bin"),
            ("HOME", "/host/home"),
            ("ANTHROPIC_API_KEY", "sk-host"),
            ("OPENAI_API_KEY", "sk-openai"),
            ("AWS_PROFILE", "bedrock"),
            ("HTTPS_PROXY", "http://proxy:8080"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_SESSION_ID", "parent-session"),
            ("CLAUDE_CODE_MESSAGING_SOCKET", "/tmp/parent.sock"),
            ("DATABASE_URL", "postgres://host/db"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
    }

    #[test]
    fn scrubbed_keeps_only_the_allowlist() {
        let temp = tempdir().unwrap();
        let env = RunEnvironment::prepare_from(
            Runner::ClaudeCode,
            temp.path(),
            EnvironmentPolicy::Scrubbed,
            None,
            &host(),
        )
        .unwrap();

        assert_eq!(env.vars.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert_eq!(env.vars.get("HOME").map(String::as_str), Some("/host/home"));
        assert_eq!(env.vars.get("ANTHROPIC_API_KEY").map(String::as_str), Some("sk-host"));
        assert_eq!(env.vars.get("AWS_PROFILE").map(String::as_str), Some("bedrock"));
        assert_eq!(
            env.vars.get("HTTPS_PROXY").map(String::as_str),
            Some("http://proxy:8080")
        );
        assert!(!env.vars.contains_key("DATABASE_URL"));
    }

    #[test]
    fn scrubbed_drops_the_launching_session_identity() {
        let temp = tempdir().unwrap();
        let env = RunEnvironment::prepare_from(
            Runner::ClaudeCode,
            temp.path(),
            EnvironmentPolicy::Scrubbed,
            None,
            &host(),
        )
        .unwrap();

        for leaked in ["CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "CLAUDE_CODE_MESSAGING_SOCKET"] {
            assert!(!env.vars.contains_key(leaked), "{leaked} must not reach a run");
        }
    }

    #[test]
    fn each_harness_gets_only_its_own_credentials() {
        let temp = tempdir().unwrap();
        let codex =
            RunEnvironment::prepare_from(Runner::Codex, temp.path(), EnvironmentPolicy::Scrubbed, None, &host())
                .unwrap();

        assert!(codex.vars.contains_key("OPENAI_API_KEY"));
        assert!(!codex.vars.contains_key("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn isolated_redirects_home_and_the_harness_config_home() {
        let temp = tempdir().unwrap();
        let run_dir = temp.path().join("run-001");
        let env =
            RunEnvironment::prepare_from(Runner::Codex, &run_dir, EnvironmentPolicy::Isolated, None, &host()).unwrap();

        let home = run_dir.join(RUN_HOME_DIR_NAME);
        assert_eq!(env.vars.get("HOME").map(String::as_str), Some(home.to_str().unwrap()));
        assert_eq!(
            env.vars.get("CODEX_HOME").map(String::as_str),
            Some(home.join(".codex").to_str().unwrap())
        );
        assert!(home.join(".codex").is_dir());
    }

    #[test]
    fn isolated_links_the_host_credentials_and_nothing_beside_them() {
        let temp = tempdir().unwrap();
        let host_config_home = temp.path().join("host-codex");
        std::fs::create_dir_all(host_config_home.join("skills/leaky")).unwrap();
        std::fs::write(host_config_home.join("auth.json"), "{}").unwrap();
        std::fs::write(host_config_home.join("config.toml"), "model = \"host\"").unwrap();
        std::fs::write(host_config_home.join("skills/leaky/SKILL.md"), "leak").unwrap();

        let mut host = host();
        host.insert(
            "CODEX_HOME".to_string(),
            host_config_home.to_string_lossy().into_owned(),
        );

        let run_dir = temp.path().join("run-001");
        let env =
            RunEnvironment::prepare_from(Runner::Codex, &run_dir, EnvironmentPolicy::Isolated, None, &host).unwrap();

        let config_home = PathBuf::from(env.vars.get("CODEX_HOME").unwrap());
        assert!(config_home.join("auth.json").symlink_metadata().unwrap().is_symlink());
        assert!(!config_home.join("config.toml").exists());
        assert!(!config_home.join("skills").exists());
    }

    #[test]
    fn isolated_pins_credential_lookup_to_the_host_config_home() {
        let temp = tempdir().unwrap();
        let env = RunEnvironment::prepare_from(
            Runner::ClaudeCode,
            temp.path(),
            EnvironmentPolicy::Isolated,
            None,
            &host(),
        )
        .unwrap();

        assert_eq!(
            env.vars.get("CLAUDE_SECURESTORAGE_CONFIG_DIR").map(String::as_str),
            Some("/host/home/.claude")
        );
    }

    #[test]
    fn isolated_respects_an_operator_set_credential_locator() {
        let temp = tempdir().unwrap();
        let mut host = host();
        host.insert(
            "CLAUDE_SECURESTORAGE_CONFIG_DIR".to_string(),
            "/host/home/other".to_string(),
        );

        let env = RunEnvironment::prepare_from(
            Runner::ClaudeCode,
            temp.path(),
            EnvironmentPolicy::Isolated,
            None,
            &host,
        )
        .unwrap();

        assert_eq!(
            env.vars.get("CLAUDE_SECURESTORAGE_CONFIG_DIR").map(String::as_str),
            Some("/host/home/other")
        );
    }

    #[test]
    fn inherited_passes_the_host_environment_through() {
        let temp = tempdir().unwrap();
        let env = RunEnvironment::prepare_from(
            Runner::ClaudeCode,
            temp.path(),
            EnvironmentPolicy::Inherited,
            None,
            &host(),
        )
        .unwrap();

        assert_eq!(env.vars, host());
    }

    /// `apply` under `Inherited` deliberately leaves the child to inherit, which is exactly
    /// the path on which a case's variables have nothing to ride in on unless they are set
    /// explicitly.
    #[test]
    fn an_inherited_run_is_still_handed_the_variables_its_case_declared() {
        let temp = tempdir().unwrap();
        let declared = CaseEnv::parse([("EVAL_SEED", "42")]).unwrap();
        let env = RunEnvironment::prepare_from(
            Runner::ClaudeCode,
            temp.path(),
            EnvironmentPolicy::Inherited,
            Some(&declared),
            &host(),
        )
        .unwrap();

        let mut command = Command::new("/usr/bin/env");
        env.apply(&mut command);

        let applied: Vec<_> = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect();

        assert_eq!(
            applied,
            vec![("EVAL_SEED".to_string(), Some("42".to_string()))],
            "the host environment is inherited rather than re-applied, and the case's own variable is the one addition"
        );
    }

    /// Under `Inherited` nothing is cleared, so there is no assembled environment for a
    /// case's variables to be folded into. Reached only through `apply`, they would be the
    /// one input that silently depends on which policy the operator picked.
    #[test]
    fn a_case_sets_its_own_variables_under_every_policy() {
        let temp = tempdir().unwrap();
        let declared = CaseEnv::parse([("EVAL_SEED", "42")]).unwrap();

        for policy in [
            EnvironmentPolicy::Inherited,
            EnvironmentPolicy::Scrubbed,
            EnvironmentPolicy::Isolated,
        ] {
            let run_dir = temp.path().join(format!("{policy:?}"));
            std::fs::create_dir_all(&run_dir).unwrap();
            let env =
                RunEnvironment::prepare_from(Runner::ClaudeCode, &run_dir, policy, Some(&declared), &host()).unwrap();

            assert_eq!(
                env.vars.get("EVAL_SEED").map(String::as_str),
                Some("42"),
                "a case declared EVAL_SEED and {policy:?} dropped it"
            );
        }
    }

    /// The allowlist decides what a run can see of this machine. A case that could name a
    /// variable outside `EVAL_*` would be editing that decision, so the refusal has to
    /// happen while the suite is read rather than after a run has already been handed the
    /// rewritten environment.
    #[test]
    fn a_case_cannot_overwrite_the_environment_the_policy_assembled() {
        assert!(
            serde_json::from_value::<CaseEnv>(serde_json::json!({ "PATH": "/evil/bin" })).is_err(),
            "a case naming PATH decides which binary the harness is"
        );
    }

    #[test]
    fn a_case_variable_that_looks_like_a_secret_stays_out_of_the_record() {
        let temp = tempdir().unwrap();
        let declared = CaseEnv::parse([("EVAL_API_KEY", "sk-case"), ("EVAL_SEED", "42")]).unwrap();
        let env = RunEnvironment::prepare_from(
            Runner::ClaudeCode,
            temp.path(),
            EnvironmentPolicy::Scrubbed,
            Some(&declared),
            &host(),
        )
        .unwrap();

        let recorded = env.recorded_vars();
        assert_eq!(recorded.get("EVAL_SEED").map(String::as_str), Some("42"));
        assert!(
            !recorded.contains_key("EVAL_API_KEY"),
            "env.json is written next to the transcript, so a case's secret is as published as the harness's"
        );
    }

    #[test]
    fn recorded_vars_leave_out_secret_values() {
        let temp = tempdir().unwrap();
        let env = RunEnvironment::prepare_from(
            Runner::ClaudeCode,
            temp.path(),
            EnvironmentPolicy::Scrubbed,
            None,
            &host(),
        )
        .unwrap();

        let recorded = env.recorded_vars();
        assert!(recorded.contains_key("PATH"));
        assert!(!recorded.contains_key("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn scrubbed_leaves_the_operator_config_home_where_the_operator_put_it() {
        let temp = tempdir().unwrap();
        let mut host = host();
        host.insert("CODEX_HOME".to_string(), "/host/home/elsewhere/.codex".to_string());

        let env =
            RunEnvironment::prepare_from(Runner::Codex, temp.path(), EnvironmentPolicy::Scrubbed, None, &host).unwrap();

        assert_eq!(
            env.vars.get("CODEX_HOME").map(String::as_str),
            Some("/host/home/elsewhere/.codex"),
            "dropping it sends the run to the default under HOME, which is nobody's config home"
        );
    }

    #[test]
    fn a_harness_gets_every_credential_variable_it_reads() {
        let temp = tempdir().unwrap();
        let mut host = host();
        host.insert("CODEX_API_KEY".to_string(), "sk-codex".to_string());
        host.insert("CODEX_ACCESS_TOKEN".to_string(), "token-codex".to_string());

        let env =
            RunEnvironment::prepare_from(Runner::Codex, temp.path(), EnvironmentPolicy::Scrubbed, None, &host).unwrap();

        assert_eq!(env.vars.get("CODEX_API_KEY").map(String::as_str), Some("sk-codex"));
        assert_eq!(
            env.vars.get("CODEX_ACCESS_TOKEN").map(String::as_str),
            Some("token-codex")
        );
    }

    #[test]
    fn isolated_takes_the_login_out_of_a_settings_file_and_leaves_the_settings() {
        let temp = tempdir().unwrap();
        let host_config_home = temp.path().join("host-cursor");
        std::fs::create_dir_all(&host_config_home).unwrap();
        std::fs::write(
            host_config_home.join("cli-config.json"),
            r#"{"authInfo":{"userId":7},"permissions":{"allow":["Bash"]},"model":{"modelId":"operator-choice"}}"#,
        )
        .unwrap();

        let mut host = host();
        host.insert("HOME".to_string(), temp.path().to_string_lossy().into_owned());
        std::fs::rename(&host_config_home, temp.path().join(".cursor")).unwrap();

        let run_dir = temp.path().join("run-001");
        let env = RunEnvironment::prepare_from(Runner::CursorAgent, &run_dir, EnvironmentPolicy::Isolated, None, &host)
            .unwrap();

        let config_home = PathBuf::from(env.vars.get("HOME").unwrap()).join(".cursor");
        let carried: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(config_home.join("cli-config.json")).unwrap()).unwrap();

        assert_eq!(carried["authInfo"]["userId"], 7);
        assert!(
            carried.get("permissions").is_none() && carried.get("model").is_none(),
            "an isolated run must not be handed the operator's permissions or model choice"
        );
    }

    #[test]
    fn cursor_agent_has_no_config_home_variable_to_redirect() {
        assert!(matches!(
            Runner::CursorAgent.config_home(),
            HarnessConfigHome::HomeRelative { .. }
        ));
    }

    #[test]
    fn scrubbed_records_the_hosts_config_home_for_every_harness() {
        for (runner, expected_path, expected_basis) in [
            (
                Runner::ClaudeCode,
                "/host/home/.claude",
                ConfigHomeBasis::Variable {
                    name: "CLAUDE_CONFIG_DIR".to_string(),
                },
            ),
            (
                Runner::Codex,
                "/host/home/.codex",
                ConfigHomeBasis::Variable {
                    name: "CODEX_HOME".to_string(),
                },
            ),
            (Runner::CursorAgent, "/host/home/.cursor", ConfigHomeBasis::HomeRelative),
        ] {
            let temp = tempdir().unwrap();
            let env =
                RunEnvironment::prepare_from(runner, temp.path(), EnvironmentPolicy::Scrubbed, None, &host()).unwrap();

            let recorded = env
                .config_home()
                .unwrap_or_else(|| panic!("{runner:?} must record where its config home was"));
            assert_eq!(recorded.origin(), ConfigHomeOrigin::Host);
            assert_eq!(recorded.path(), Path::new(expected_path));
            assert_eq!(recorded.basis(), &expected_basis);
        }
    }

    #[test]
    fn isolated_records_the_run_config_home_and_not_the_hosts() {
        for runner in [Runner::ClaudeCode, Runner::Codex, Runner::CursorAgent] {
            let temp = tempdir().unwrap();
            let run_dir = temp.path().join("run-001");
            let env =
                RunEnvironment::prepare_from(runner, &run_dir, EnvironmentPolicy::Isolated, None, &host()).unwrap();

            let recorded = env
                .config_home()
                .unwrap_or_else(|| panic!("{runner:?} must record its per-run config home"));
            assert_eq!(recorded.origin(), ConfigHomeOrigin::Run);
            assert!(
                recorded.path().starts_with(&run_dir),
                "{runner:?} recorded {} instead of a path under the run directory",
                recorded.path().display()
            );
            assert_ne!(
                recorded.path(),
                Path::new("/host/home").join(match runner.config_home() {
                    HarnessConfigHome::Redirectable { dir_name, .. } | HarnessConfigHome::HomeRelative { dir_name } =>
                        dir_name,
                })
            );
        }
    }

    /// `--out-dir` is routinely given relative, which is why this changes the process's
    /// working directory rather than only handing `prepare_from` a relative `PathBuf`:
    /// the bug this guards against only appears once resolving the path actually depends
    /// on the working directory the run started from.
    #[test]
    fn isolated_absolutizes_a_relative_run_dir_before_recording_its_config_home() {
        let temp = tempdir().unwrap();
        let original_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(temp.path()).unwrap();

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let run_dir = Path::new("run-001");
            let env = RunEnvironment::prepare_from(Runner::Codex, run_dir, EnvironmentPolicy::Isolated, None, &host())
                .unwrap();

            let recorded = env
                .config_home()
                .unwrap_or_else(|| panic!("a relative run_dir must still record a config home"));
            assert!(
                recorded.path().is_absolute(),
                "recorded {} from a relative run_dir",
                recorded.path().display()
            );

            let expected_suffix = run_dir
                .join(RUN_HOME_DIR_NAME)
                .join(Runner::Codex.config_home().dir_name());
            assert!(
                recorded.path().ends_with(&expected_suffix),
                "recorded {} instead of the run's own config home",
                recorded.path().display()
            );
        }));

        std::env::set_current_dir(original_cwd).unwrap();
        outcome.unwrap();
    }

    #[test]
    fn env_record_carries_the_config_home_alongside_the_vars() {
        let temp = tempdir().unwrap();
        let env = RunEnvironment::prepare_from(Runner::Codex, temp.path(), EnvironmentPolicy::Scrubbed, None, &host())
            .unwrap();

        let record = env.record();
        assert_eq!(record.vars.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert_eq!(
            record.config_home.map(|home| home.path().to_path_buf()),
            Some(PathBuf::from("/host/home/.codex"))
        );
    }
}
