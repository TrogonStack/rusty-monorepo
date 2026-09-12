//! The process environment a single eval run is executed in.
//!
//! A run inherits nothing by accident. Everything the harness subprocess can see is
//! either on an explicit allowlist or was put there for this run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::Runner;
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
}

impl RunEnvironment {
    pub fn prepare(runner: Runner, run_dir: &Path, policy: EnvironmentPolicy) -> std::io::Result<Self> {
        Self::prepare_from(runner, run_dir, policy, &host_environment())
    }

    fn prepare_from(
        runner: Runner,
        run_dir: &Path,
        policy: EnvironmentPolicy,
        host: &BTreeMap<String, String>,
    ) -> std::io::Result<Self> {
        if matches!(policy, EnvironmentPolicy::Inherited) {
            return Ok(Self {
                policy,
                vars: host.clone(),
            });
        }

        let mut vars = allowlisted(runner, host);

        if matches!(policy, EnvironmentPolicy::Isolated) {
            let host_config_home = runner.config_home().resolve_on_host(host);

            let home = run_dir.join(RUN_HOME_DIR_NAME);
            std::fs::create_dir_all(&home)?;
            vars.insert("HOME".to_string(), path_string(&home));

            let config_home = home.join(runner.config_home().dir_name());
            std::fs::create_dir_all(&config_home)?;
            if let HarnessConfigHome::Redirectable { var, .. } = runner.config_home() {
                vars.insert(var.to_string(), path_string(&config_home));
            }

            if let Some(host_config_home) = host_config_home {
                link_auth_entries(runner, &host_config_home, &config_home)?;
                if let Some(var) = runner.credential_locator_var() {
                    vars.entry(var.to_string())
                        .or_insert_with(|| path_string(&host_config_home));
                }
            }
        }

        Ok(Self { policy, vars })
    }

    pub fn policy(&self) -> EnvironmentPolicy {
        self.policy
    }

    pub fn apply(&self, command: &mut Command) {
        if matches!(self.policy, EnvironmentPolicy::Inherited) {
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
        let env = RunEnvironment::prepare_from(Runner::ClaudeCode, temp.path(), EnvironmentPolicy::Scrubbed, &host())
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
        let env = RunEnvironment::prepare_from(Runner::ClaudeCode, temp.path(), EnvironmentPolicy::Scrubbed, &host())
            .unwrap();

        for leaked in ["CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "CLAUDE_CODE_MESSAGING_SOCKET"] {
            assert!(!env.vars.contains_key(leaked), "{leaked} must not reach a run");
        }
    }

    #[test]
    fn each_harness_gets_only_its_own_credentials() {
        let temp = tempdir().unwrap();
        let codex =
            RunEnvironment::prepare_from(Runner::Codex, temp.path(), EnvironmentPolicy::Scrubbed, &host()).unwrap();

        assert!(codex.vars.contains_key("OPENAI_API_KEY"));
        assert!(!codex.vars.contains_key("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn isolated_redirects_home_and_the_harness_config_home() {
        let temp = tempdir().unwrap();
        let run_dir = temp.path().join("run-001");
        let env = RunEnvironment::prepare_from(Runner::Codex, &run_dir, EnvironmentPolicy::Isolated, &host()).unwrap();

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
        let env = RunEnvironment::prepare_from(Runner::Codex, &run_dir, EnvironmentPolicy::Isolated, &host).unwrap();

        let config_home = PathBuf::from(env.vars.get("CODEX_HOME").unwrap());
        assert!(config_home.join("auth.json").symlink_metadata().unwrap().is_symlink());
        assert!(!config_home.join("config.toml").exists());
        assert!(!config_home.join("skills").exists());
    }

    #[test]
    fn isolated_pins_credential_lookup_to_the_host_config_home() {
        let temp = tempdir().unwrap();
        let env = RunEnvironment::prepare_from(Runner::ClaudeCode, temp.path(), EnvironmentPolicy::Isolated, &host())
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

        let env =
            RunEnvironment::prepare_from(Runner::ClaudeCode, temp.path(), EnvironmentPolicy::Isolated, &host).unwrap();

        assert_eq!(
            env.vars.get("CLAUDE_SECURESTORAGE_CONFIG_DIR").map(String::as_str),
            Some("/host/home/other")
        );
    }

    #[test]
    fn inherited_passes_the_host_environment_through() {
        let temp = tempdir().unwrap();
        let env = RunEnvironment::prepare_from(Runner::ClaudeCode, temp.path(), EnvironmentPolicy::Inherited, &host())
            .unwrap();

        assert_eq!(env.vars, host());
    }

    #[test]
    fn recorded_vars_leave_out_secret_values() {
        let temp = tempdir().unwrap();
        let env = RunEnvironment::prepare_from(Runner::ClaudeCode, temp.path(), EnvironmentPolicy::Scrubbed, &host())
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

        let env = RunEnvironment::prepare_from(Runner::Codex, temp.path(), EnvironmentPolicy::Scrubbed, &host).unwrap();

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

        let env = RunEnvironment::prepare_from(Runner::Codex, temp.path(), EnvironmentPolicy::Scrubbed, &host).unwrap();

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
        let env =
            RunEnvironment::prepare_from(Runner::CursorAgent, &run_dir, EnvironmentPolicy::Isolated, &host).unwrap();

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
}
