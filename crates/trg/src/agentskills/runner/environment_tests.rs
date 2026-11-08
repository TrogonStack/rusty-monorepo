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
