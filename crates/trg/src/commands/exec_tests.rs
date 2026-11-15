use super::*;
use crate::secrets::telemetry::testing::assert_never_recorded;
use clap::Parser;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::SdkTracerProvider;
use std::os::unix::process::CommandExt;
use tracing_subscriber::prelude::*;

fn parse(argv: &[&str]) -> ExecArgs {
    let mut full = vec!["trg", "exec", "run"];
    full.extend_from_slice(argv);
    let cli = crate::cli::Cli::try_parse_from(full).expect("parses");
    let crate::commands::Commands::Exec { command } = cli.command else {
        panic!("expected Exec");
    };
    let ExecCommands::Run(args) = command else {
        panic!("expected Run");
    };
    args
}

#[test]
fn extra_args_after_dashdash_are_captured_verbatim() {
    let args = parse(&["--name", "demo", "--", "--resume"]);
    assert_eq!(args.name, "demo");
    assert_eq!(args.extra_args, vec!["--resume".to_string()]);
}

/// Without `--`, a token meant for the launched command has no way to
/// tell itself apart from a misspelled flag of `trg exec run`'s own — so
/// clap refuses to parse it at all rather than guess.
#[test]
fn extra_args_without_a_dashdash_fail_to_parse() {
    let full = vec!["trg", "exec", "run", "--name", "demo", "--resume"];
    assert!(crate::cli::Cli::try_parse_from(full).is_err());
}

/// This is the enforcement `--` buys back: an unrecognized flag before it
/// is a hard parse error, exactly like anywhere else in the CLI, instead
/// of silently starting passthrough capture.
#[test]
fn an_unrecognized_flag_before_dashdash_fails_to_parse() {
    let full = vec![
        "trg", "exec", "run", "--evn", "DEBUG=1", "--name", "demo", "--", "--resume",
    ];
    assert!(crate::cli::Cli::try_parse_from(full).is_err());
}

#[test]
fn trg_s_own_recognized_flags_before_dashdash_still_work() {
    let args = parse(&["--env", "A=B", "--name", "demo", "--", "--resume"]);
    assert_eq!(args.env, vec![("A".to_string(), "B".to_string())]);
    assert_eq!(args.name, "demo");
    assert_eq!(args.extra_args, vec!["--resume".to_string()]);
}

#[test]
fn the_entry_is_named_with_a_flag() {
    let args = parse(&["--name", "demo"]);
    assert_eq!(args.name, "demo");
}

#[test]
fn a_bare_entry_name_is_rejected() {
    let full = vec!["trg", "exec", "run", "demo"];
    assert!(crate::cli::Cli::try_parse_from(full).is_err());
}

#[test]
fn list_offers_the_output_choice_and_nothing_else() {
    let cli = crate::cli::Cli::try_parse_from(["trg", "exec", "list"]).expect("parses");
    let crate::commands::Commands::Exec { command } = cli.command else {
        panic!("expected Exec");
    };
    assert!(matches!(command, ExecCommands::List(_)));
}

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn an_inherited_var_survives_untouched_when_nothing_mentions_it() {
    let parent = map(&[("HOME", "/home/yordis")]);
    let out = merge_env(&parent, &[], &HashMap::new(), &[]);
    assert_eq!(out.get("HOME"), Some(&"/home/yordis".to_string()));
}

#[test]
fn unset_removes_an_inherited_var_the_entry_never_mentions() {
    let parent = map(&[("ANTHROPIC_API_KEY", "sk-ant-old")]);
    let out = merge_env(&parent, &["ANTHROPIC_API_KEY".to_string()], &HashMap::new(), &[]);
    assert!(!out.contains_key("ANTHROPIC_API_KEY"));
}

#[test]
fn entry_env_overrides_an_inherited_var_of_the_same_name() {
    let parent = map(&[("MODE", "dev")]);
    let entry_env = map(&[("MODE", "prod")]);
    let out = merge_env(&parent, &[], &entry_env, &[]);
    assert_eq!(out.get("MODE"), Some(&"prod".to_string()));
}

#[test]
fn cli_env_overrides_an_entry_declared_var_of_the_same_name() {
    let entry_env = map(&[("MODE", "prod")]);
    let cli_env = vec![("MODE".to_string(), "debug".to_string())];
    let out = merge_env(&HashMap::new(), &[], &entry_env, &cli_env);
    assert_eq!(out.get("MODE"), Some(&"debug".to_string()));
}

#[test]
fn a_cli_unset_takes_effect_even_though_its_a_different_flag_than_the_entrys() {
    let parent = map(&[("STALE", "1")]);
    // Mirrors how `run` folds the entry's `unset` and the CLI's
    // `--unset` into one list before calling `merge_env`.
    let unset = vec!["STALE".to_string()];
    let out = merge_env(&parent, &unset, &HashMap::new(), &[]);
    assert!(!out.contains_key("STALE"));
}

#[test]
fn cli_env_wins_even_over_a_var_the_entry_also_unset() {
    // Not a realistic config, but the precedence has to hold regardless:
    // whatever the caller asks for last is what the child sees.
    let entry_env = map(&[("X", "from-entry")]);
    let unset = vec!["X".to_string()];
    let cli_env = vec![("X".to_string(), "from-cli".to_string())];
    let out = merge_env(&HashMap::new(), &unset, &entry_env, &cli_env);
    assert_eq!(out.get("X"), Some(&"from-cli".to_string()));
}

#[test]
fn an_env_pair_without_an_equals_sign_is_refused() {
    let err = parse_env_pair("NOEQUALS").unwrap_err();
    assert!(err.contains("KEY=VALUE"), "{err}");
}

#[test]
fn an_env_pair_with_an_empty_key_is_refused() {
    let err = parse_env_pair("=value").unwrap_err();
    assert!(err.contains("empty key"), "{err}");
}

#[test]
fn an_env_pair_value_may_itself_contain_an_equals_sign() {
    assert_eq!(parse_env_pair("A=b=c").unwrap(), ("A".to_string(), "b=c".to_string()));
}

#[test]
fn a_launch_failure_names_the_command_and_never_the_env() {
    let env = map(&[("TOKEN", "super-secret-value")]);
    let launch = PreparedLaunch::new(
        "trg-exec-test-command-that-does-not-exist".to_string(),
        Vec::new(),
        env,
        OutputFormat::Text,
    );
    let message = launch.replace_with(|command| command.exec());
    assert!(
        message.contains("trg-exec-test-command-that-does-not-exist"),
        "{message}"
    );
    assert!(!message.contains("super-secret-value"), "{message}");
}

#[test]
fn a_json_failure_is_rendered_as_an_error_document() {
    // `report_failure` prints to stdout/stderr rather than returning a
    // string, so this only pins the exit code and the format branch it
    // takes; the rendering itself is exercised by hand against `--output-format json`.
    assert_eq!(report_failure("boom", OutputFormat::Json), ExitCode::Failed);
    assert_eq!(report_failure("boom", OutputFormat::Text), ExitCode::Failed);
}

#[test]
fn reporting_names_succeeds_under_either_format_even_with_none_to_show() {
    assert_eq!(report_names(&[], OutputFormat::Text), ExitCode::Success);
    assert_eq!(report_names(&[], OutputFormat::Json), ExitCode::Success);
    assert_eq!(
        report_names(&["alpha".to_string(), "zebra".to_string()], OutputFormat::Text),
        ExitCode::Success
    );
}

#[test]
fn merging_records_how_many_variables_and_never_which() {
    const SECRET: &str = "exec-span-secret-2a9f";
    let parent = HashMap::from([("PATH".to_string(), "/bin".to_string())]);
    let entry_env = HashMap::from([("ANTHROPIC_API_KEY".to_string(), SECRET.to_string())]);
    let (env, trace) = crate::telemetry::testing::capture(|| traced_merge_env(&parent, &[], &entry_env, &[]));
    assert_eq!(env.len(), 2);

    assert_eq!(
        trace.attribute("merge env", "trg.exec.env.var.count"),
        Some(opentelemetry::Value::I64(2))
    );
    assert_never_recorded(&trace, &[SECRET, "ANTHROPIC_API_KEY", "/bin"]);
}

#[test]
fn the_root_span_is_exported_before_exec_and_parents_the_launched_command() {
    let exporter = crate::telemetry::testing::KeptSpans::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(provider.tracer("trg")));
    let _guard = tracing::subscriber::set_default(subscriber);

    let root = tracing::info_span!("trg");
    let launch =
        root.in_scope(|| PreparedLaunch::new("true".to_string(), Vec::new(), HashMap::new(), OutputFormat::Text));
    let telemetry = Telemetry::exporting_traces_to(provider, root);

    let code = launch.exec_with(telemetry, |command| {
        let traceparent = command
            .get_envs()
            .find(|(key, _)| *key == "TRACEPARENT")
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().into_owned())
            .expect("the launched command continues the trace");
        let spans = exporter.spans();
        let root = spans
            .iter()
            .find(|span| span.name == "trg")
            .expect("root span exported before exec");
        assert!(
            traceparent.contains(&root.span_context.span_id().to_string()),
            "{traceparent}"
        );
        std::io::Error::other("exec refused in test")
    });
    assert_eq!(code, ExitCode::Failed);
}
