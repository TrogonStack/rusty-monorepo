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
        RunEnvironment::prepare_from(Runner::Codex, temp.path(), EnvironmentPolicy::Scrubbed, None, &host()).unwrap();

    assert!(codex.vars.contains_key("OPENAI_API_KEY"));
    assert!(!codex.vars.contains_key("ANTHROPIC_API_KEY"));
}

#[test]
fn isolated_redirects_home_and_the_harness_config_home() {
    let temp = tempdir().unwrap();
    // Resolved, as the run home is: the system temp directory may sit behind a link.
    let run_dir = std::fs::canonicalize(temp.path()).unwrap().join("run-001");
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
    let env = RunEnvironment::prepare_from(Runner::Codex, &run_dir, EnvironmentPolicy::Isolated, None, &host).unwrap();

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
        let env = RunEnvironment::prepare_from(Runner::ClaudeCode, &run_dir, policy, Some(&declared), &host()).unwrap();

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
    let env =
        RunEnvironment::prepare_from(Runner::CursorAgent, &run_dir, EnvironmentPolicy::Isolated, None, &host).unwrap();

    let config_home = PathBuf::from(env.vars.get("HOME").unwrap()).join(".cursor");
    let carried: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config_home.join("cli-config.json")).unwrap()).unwrap();

    assert_eq!(carried["authInfo"]["userId"], 7);
    assert!(
        carried.get("permissions").is_none() && carried.get("model").is_none(),
        "an isolated run must not be handed the operator's permissions or model choice"
    );
}

/// trogonai-agent keeps the providers a run signs in through in the same `settings.json` as the
/// permissions, hooks, and saved models an isolated run holds still.
#[test]
fn an_isolated_trogonai_agent_run_keeps_only_the_route_it_signs_in_through() {
    let temp = tempdir().unwrap();
    let host_config_home = temp.path().join(".trogonai-agent");
    std::fs::create_dir_all(&host_config_home).unwrap();
    std::fs::write(
        host_config_home.join("settings.json"),
        r#"{"providers":{"gateway":{"protocol":"openai-chat-completions","base_url":"https://gateway.example/v1","auth":{"type":"bearer","env":"GATEWAY_KEY"}}},"provider":"gateway","models":{"gateway":"operator-choice"},"permission":{"bash":"allow"}}"#,
    )
    .unwrap();

    let mut host = host();
    host.insert("HOME".to_string(), temp.path().to_string_lossy().into_owned());

    let run_dir = std::fs::canonicalize(temp.path()).unwrap().join("run-001");
    let env = RunEnvironment::prepare_from(
        Runner::TrogonaiAgent,
        &run_dir,
        EnvironmentPolicy::Isolated,
        None,
        &host,
    )
    .unwrap();

    let config_home = PathBuf::from(env.vars.get("TROGONAI_AGENT_HOME").unwrap());
    assert!(config_home.starts_with(&run_dir));
    let carried: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config_home.join("settings.json")).unwrap()).unwrap();

    assert_eq!(carried["provider"], "gateway");
    assert_eq!(carried["providers"]["gateway"]["auth"]["env"], "GATEWAY_KEY");
    assert!(
        carried.get("permission").is_none() && carried.get("models").is_none(),
        "an isolated run must not be handed the operator's permissions or model choice"
    );
}

/// A provider the operator declared reads its key from a variable of the operator's
/// choosing, so no fixed list could have admitted it.
#[test]
fn a_scrubbed_trogonai_agent_run_sees_the_key_its_declared_provider_names_and_nothing_else() {
    let temp = tempdir().unwrap();
    let host_config_home = temp.path().join("profile");
    std::fs::create_dir_all(&host_config_home).unwrap();
    std::fs::write(
        host_config_home.join("settings.json"),
        r#"{"providers":{"gateway":{"protocol":"openai-chat-completions","base_url":"https://gateway.example/v1","auth":{"type":"bearer","env":"GATEWAY_KEY"}},"local":{"protocol":"openai-chat-completions","base_url":"http://127.0.0.1:8080/v1","auth":{"type":"none"}}}}"#,
    )
    .unwrap();

    let mut host = host();
    host.insert(
        "TROGONAI_AGENT_HOME".to_string(),
        host_config_home.to_string_lossy().into_owned(),
    );
    host.insert("GATEWAY_KEY".to_string(), "secret".to_string());
    host.insert("UNRELATED_KEY".to_string(), "other".to_string());

    let env = RunEnvironment::prepare_from(
        Runner::TrogonaiAgent,
        temp.path(),
        EnvironmentPolicy::Scrubbed,
        None,
        &host,
    )
    .unwrap();

    assert_eq!(env.vars.get("GATEWAY_KEY").map(String::as_str), Some("secret"));
    assert!(!env.vars.contains_key("UNRELATED_KEY"));
}

/// A declared key variable is named by the operator, so it need not end in a suffix the
/// name-based redaction knows; `env.json` must leave it out all the same.
#[test]
fn a_declared_key_is_handed_to_the_harness_but_never_recorded() {
    let temp = tempdir().unwrap();
    let host_config_home = temp.path().join("profile");
    std::fs::create_dir_all(&host_config_home).unwrap();
    std::fs::write(
        host_config_home.join("settings.json"),
        r#"{"providers":{"gateway":{"protocol":"openai-chat-completions","base_url":"https://gateway.example/v1","auth":{"type":"bearer","env":"TROGONAI_AGENT_LOGIN"}}}}"#,
    )
    .unwrap();

    let mut host = host();
    host.insert(
        "TROGONAI_AGENT_HOME".to_string(),
        host_config_home.to_string_lossy().into_owned(),
    );
    host.insert("TROGONAI_AGENT_LOGIN".to_string(), "secret".to_string());

    for policy in [
        EnvironmentPolicy::Scrubbed,
        EnvironmentPolicy::Isolated,
        EnvironmentPolicy::Inherited,
    ] {
        let env =
            RunEnvironment::prepare_from(Runner::TrogonaiAgent, &temp.path().join("run"), policy, None, &host).unwrap();
        assert_eq!(
            env.vars.get("TROGONAI_AGENT_LOGIN").map(String::as_str),
            Some("secret"),
            "{policy:?}"
        );
        assert!(!env.recorded_vars().contains_key("TROGONAI_AGENT_LOGIN"), "{policy:?}");
    }
}

/// The AI Gateway key is linked from inside the run's config home, so a relative host
/// config home has to be made absolute first or the link resolves to nothing.
#[test]
fn an_isolated_trogonai_agent_run_links_the_key_of_a_relative_host_config_home() {
    let test_name =
        "agentskills::runner::environment::tests::an_isolated_trogonai_agent_run_links_the_key_of_a_relative_host_config_home";
    let child_marker = "TRG_TEST_RELATIVE_TROGONAI_AGENT_HOME_CHILD";
    if std::env::var_os(child_marker).is_none() {
        let temp = tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env(child_marker, "1")
            .current_dir(temp.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("1 passed; 0 failed"), "{stdout}");
        assert!(stdout.contains("relative trogonai-agent home verified"), "{stdout}");
        return;
    }

    std::fs::create_dir_all("profile").unwrap();
    std::fs::write("profile/api-key", "gateway-key").unwrap();
    let mut host = host();
    host.insert("TROGONAI_AGENT_HOME".to_string(), "profile".to_string());

    let env = RunEnvironment::prepare_from(
        Runner::TrogonaiAgent,
        Path::new("run-001"),
        EnvironmentPolicy::Isolated,
        None,
        &host,
    )
    .unwrap();
    let config_home = PathBuf::from(env.vars.get("TROGONAI_AGENT_HOME").unwrap());
    assert_eq!(
        std::fs::read_to_string(config_home.join("api-key")).unwrap(),
        "gateway-key"
    );
    println!("relative trogonai-agent home verified");
}

/// A pi `models.json` reads a provider's key through `$NAME` or `${NAME}`, so the variable
/// is the operator's choice; `$$` is a literal dollar sign and names nothing.
#[test]
fn a_pi_run_sees_the_variables_its_models_file_reads_and_nothing_else() {
    let temp = tempdir().unwrap();
    let config_dir = temp.path().join("pi");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("models.json"),
        r#"{"providers":{"gateway":{"baseUrl":"https://gateway.example/v1","api":"openai-responses","apiKey":"$GATEWAY_KEY","headers":{"X-Team":"${TEAM_TOKEN}","X-Price":"$$5"}}}}"#,
    )
    .unwrap();
    std::fs::write(config_dir.join("settings.json"), r#"{"defaultModel":"gateway/m"}"#).unwrap();

    let mut host = host();
    host.insert(
        "PI_CODING_AGENT_DIR".to_string(),
        config_dir.to_string_lossy().into_owned(),
    );
    host.insert("GATEWAY_KEY".to_string(), "secret".to_string());
    host.insert("TEAM_TOKEN".to_string(), "team".to_string());
    host.insert("UNRELATED_KEY".to_string(), "other".to_string());

    let run_dir = temp.path().join("run-001");
    let env = RunEnvironment::prepare_from(Runner::Pi, &run_dir, EnvironmentPolicy::Isolated, None, &host).unwrap();
    assert_eq!(env.vars.get("GATEWAY_KEY").map(String::as_str), Some("secret"));
    assert_eq!(env.vars.get("TEAM_TOKEN").map(String::as_str), Some("team"));
    assert!(!env.vars.contains_key("UNRELATED_KEY"));
    assert!(!env.recorded_vars().contains_key("GATEWAY_KEY"));

    let run_config = PathBuf::from(env.vars.get("PI_CODING_AGENT_DIR").unwrap());
    assert!(run_config.join("models.json").exists());
    assert!(
        !run_config.join("settings.json").exists(),
        "an isolated run keeps the routes and keys, not the operator's settings"
    );
}

/// An opencode provider reads its key through `{env:NAME}` in the config, so the variable
/// is the operator's choice and no fixed list could admit it.
#[test]
fn an_opencode_run_sees_the_variables_its_config_reads_and_nothing_else() {
    let temp = tempdir().unwrap();
    let config_dir = temp.path().join("opencode");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("opencode.json"),
        r#"{"provider":{"gateway":{"npm":"@ai-sdk/openai","options":{"baseURL":"https://gateway.example/v1","apiKey":"{env:GATEWAY_KEY}"}}},"model":"gateway/m","permission":{"bash":"allow"}}"#,
    )
    .unwrap();

    let mut host = host();
    host.insert(
        "OPENCODE_CONFIG_DIR".to_string(),
        config_dir.to_string_lossy().into_owned(),
    );
    host.insert("GATEWAY_KEY".to_string(), "secret".to_string());
    host.insert("UNRELATED_KEY".to_string(), "other".to_string());

    let run_dir = temp.path().join("run-001");
    let env =
        RunEnvironment::prepare_from(Runner::OpenCode, &run_dir, EnvironmentPolicy::Isolated, None, &host).unwrap();
    assert_eq!(env.vars.get("GATEWAY_KEY").map(String::as_str), Some("secret"));
    assert!(!env.vars.contains_key("UNRELATED_KEY"));
    assert!(!env.recorded_vars().contains_key("GATEWAY_KEY"));

    let run_config = PathBuf::from(env.vars.get("OPENCODE_CONFIG_DIR").unwrap());
    let carried: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_config.join("opencode.json")).unwrap()).unwrap();
    assert_eq!(carried["model"], "gateway/m");
    assert!(
        carried.get("permission").is_none(),
        "an isolated run keeps only the route"
    );
}

#[test]
fn a_trogonai_agent_profile_that_cannot_be_read_admits_no_declared_key() {
    let temp = tempdir().unwrap();
    let host_config_home = temp.path().join("profile");
    std::fs::create_dir_all(&host_config_home).unwrap();
    std::fs::write(host_config_home.join("settings.json"), "not json").unwrap();

    let mut host = host();
    host.insert(
        "TROGONAI_AGENT_HOME".to_string(),
        host_config_home.to_string_lossy().into_owned(),
    );
    host.insert("GATEWAY_KEY".to_string(), "secret".to_string());

    let env = RunEnvironment::prepare_from(
        Runner::TrogonaiAgent,
        temp.path(),
        EnvironmentPolicy::Scrubbed,
        None,
        &host,
    )
    .unwrap();

    assert!(!env.vars.contains_key("GATEWAY_KEY"));
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
        (
            Runner::TrogonaiAgent,
            "/host/home/.trogonai-agent",
            ConfigHomeBasis::Variable {
                name: "TROGONAI_AGENT_HOME".to_string(),
            },
        ),
        (
            Runner::OpenCode,
            "/host/home/.config/opencode",
            ConfigHomeBasis::Variable {
                name: "OPENCODE_CONFIG_DIR".to_string(),
            },
        ),
        (
            Runner::Pi,
            "/host/home/.pi/agent",
            ConfigHomeBasis::Variable {
                name: "PI_CODING_AGENT_DIR".to_string(),
            },
        ),
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
    for runner in [
        Runner::ClaudeCode,
        Runner::Codex,
        Runner::CursorAgent,
        Runner::TrogonaiAgent,
        Runner::OpenCode,
        Runner::Pi,
    ] {
        let temp = tempdir().unwrap();
        let run_dir = std::fs::canonicalize(temp.path()).unwrap().join("run-001");
        let env = RunEnvironment::prepare_from(runner, &run_dir, EnvironmentPolicy::Isolated, None, &host()).unwrap();

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

/// A harness that keeps private state may refuse a home it reaches through a symlink, as
/// trogonai-agent does under macOS's `/tmp`, so the run is handed the resolved path.
#[cfg(unix)]
#[test]
fn isolated_hands_the_harness_a_home_with_no_symlink_on_the_way_to_it() {
    let temp = tempdir().unwrap();
    let real = temp.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let env = RunEnvironment::prepare_from(
        Runner::Codex,
        &link.join("run-001"),
        EnvironmentPolicy::Isolated,
        None,
        &host(),
    )
    .unwrap();

    let home = PathBuf::from(env.vars.get("HOME").unwrap());
    assert_eq!(
        home,
        std::fs::canonicalize(real.join("run-001").join(RUN_HOME_DIR_NAME)).unwrap()
    );
    let config_home = PathBuf::from(env.vars.get("CODEX_HOME").unwrap());
    assert!(config_home.starts_with(&home), "{}", config_home.display());
}

#[test]
fn isolated_absolutizes_a_relative_run_dir_before_recording_its_config_home() {
    let test_name = "agentskills::runner::environment::tests::isolated_absolutizes_a_relative_run_dir_before_recording_its_config_home";
    let child_marker = "TRG_TEST_RELATIVE_RUN_DIR_CHILD";
    if std::env::var_os(child_marker).is_none() {
        let temp = tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env(child_marker, "1")
            .current_dir(temp.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("1 passed; 0 failed"), "{stdout}");
        assert!(stdout.contains("relative config home verified"), "{stdout}");
        return;
    }

    let run_dir = Path::new("run-001");
    let env = RunEnvironment::prepare_from(Runner::Codex, run_dir, EnvironmentPolicy::Isolated, None, &host()).unwrap();
    let recorded = env.config_home().expect("a relative run_dir records a config home");
    assert!(recorded.path().is_absolute(), "{}", recorded.path().display());
    let expected = std::env::current_dir()
        .unwrap()
        .join(run_dir)
        .join(RUN_HOME_DIR_NAME)
        .join(Runner::Codex.config_home().dir_name());
    assert_eq!(recorded.path(), expected);
    assert_eq!(
        env.vars.get("CODEX_HOME").map(PathBuf::from),
        Some(expected.clone()),
        "the harness starts in its workspace, so the variable it reads must not be relative"
    );
    assert_eq!(
        env.vars.get("HOME").map(PathBuf::from),
        expected.parent().map(Path::to_path_buf)
    );
    println!("relative config home verified");
}

#[test]
fn env_record_carries_the_config_home_alongside_the_vars() {
    let temp = tempdir().unwrap();
    let env =
        RunEnvironment::prepare_from(Runner::Codex, temp.path(), EnvironmentPolicy::Scrubbed, None, &host()).unwrap();

    let record = env.record();
    assert_eq!(record.vars.get("PATH").map(String::as_str), Some("/usr/bin"));
    assert_eq!(
        record.config_home.map(|home| home.path().to_path_buf()),
        Some(PathBuf::from("/host/home/.codex"))
    );
}

fn otel_host() -> BTreeMap<String, String> {
    let mut host = host();
    host.insert(
        "OTEL_EXPORTER_OTLP_ENDPOINT".to_string(),
        "http://collector:4318".to_string(),
    );
    host.insert(
        "OTEL_EXPORTER_OTLP_HEADERS".to_string(),
        "authorization=Bearer abc".to_string(),
    );
    host
}

fn applied(env: &RunEnvironment) -> BTreeMap<String, String> {
    let mut command = Command::new("true");
    env.apply(&mut command);
    command
        .get_envs()
        .filter_map(|(key, value)| Some((key.to_str()?.to_string(), value?.to_str()?.to_string())))
        .collect()
}

fn forwarded(runner: Runner, policy: EnvironmentPolicy, forwarding: TelemetryForwarding) -> RunEnvironment {
    let temp = tempdir().unwrap();
    let host = otel_host();
    let mut env = RunEnvironment::prepare_from(runner, &temp.path().join("run"), policy, None, &host).unwrap();
    env.forward_telemetry(runner, forwarding, &host, ContentCapture::NoContent);
    env
}

#[test]
fn telemetry_stays_out_of_every_policy_unless_forwarded() {
    for policy in [
        EnvironmentPolicy::Inherited,
        EnvironmentPolicy::Scrubbed,
        EnvironmentPolicy::Isolated,
    ] {
        let vars = applied(&forwarded(Runner::ClaudeCode, policy, TelemetryForwarding::Off));
        assert!(!vars.contains_key("CLAUDE_CODE_ENABLE_TELEMETRY"), "{policy:?}");
        assert!(!vars.contains_key("OTEL_EXPORTER_OTLP_ENDPOINT"), "{policy:?}");
    }
}

#[test]
fn forwarded_telemetry_reaches_claude_code_under_every_policy() {
    for policy in [
        EnvironmentPolicy::Inherited,
        EnvironmentPolicy::Scrubbed,
        EnvironmentPolicy::Isolated,
    ] {
        let vars = applied(&forwarded(Runner::ClaudeCode, policy, TelemetryForwarding::On));
        assert_eq!(
            vars.get("CLAUDE_CODE_ENABLE_TELEMETRY").map(String::as_str),
            Some("1"),
            "{policy:?}"
        );
        if !matches!(policy, EnvironmentPolicy::Inherited) {
            assert_eq!(
                vars.get("OTEL_EXPORTER_OTLP_ENDPOINT").map(String::as_str),
                Some("http://collector:4318"),
                "{policy:?}"
            );
        }
    }
}

#[test]
fn forwarded_exporter_headers_stay_out_of_the_record() {
    for policy in [EnvironmentPolicy::Inherited, EnvironmentPolicy::Isolated] {
        let env = forwarded(Runner::ClaudeCode, policy, TelemetryForwarding::On);
        let recorded = env.recorded_vars();
        assert!(!recorded.contains_key("OTEL_EXPORTER_OTLP_HEADERS"), "{policy:?}");
        assert!(recorded.contains_key("CLAUDE_CODE_ENABLE_TELEMETRY"), "{policy:?}");
    }
}

#[test]
fn claude_code_exports_its_own_traces_only_when_forwarded_a_traces_endpoint() {
    for policy in [EnvironmentPolicy::Inherited, EnvironmentPolicy::Isolated] {
        assert!(
            forwarded(Runner::ClaudeCode, policy, TelemetryForwarding::On).harness_exports_traces(),
            "{policy:?}"
        );
        assert!(
            !forwarded(Runner::ClaudeCode, policy, TelemetryForwarding::Off).harness_exports_traces(),
            "{policy:?}"
        );

        let temp = tempdir().unwrap();
        let host = host();
        let mut env =
            RunEnvironment::prepare_from(Runner::ClaudeCode, &temp.path().join("run"), policy, None, &host).unwrap();
        env.forward_telemetry(
            Runner::ClaudeCode,
            TelemetryForwarding::On,
            &host,
            ContentCapture::NoContent,
        );
        assert!(applied(&env).contains_key("CLAUDE_CODE_ENABLE_TELEMETRY"), "{policy:?}");
        assert!(!env.harness_exports_traces(), "{policy:?}");
    }
}

#[test]
fn only_a_forwarding_codex_run_gets_an_otel_table() {
    assert!(
        forwarded(Runner::Codex, EnvironmentPolicy::Isolated, TelemetryForwarding::On)
            .codex_otel()
            .is_some()
    );
    assert!(
        forwarded(Runner::Codex, EnvironmentPolicy::Isolated, TelemetryForwarding::Off)
            .codex_otel()
            .is_none()
    );
    assert!(
        forwarded(Runner::ClaudeCode, EnvironmentPolicy::Isolated, TelemetryForwarding::On)
            .codex_otel()
            .is_none()
    );
}
