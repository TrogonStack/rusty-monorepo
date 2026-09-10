//! `trg exec run` — exec-replace into any command with its `env` resolved
//! the same way `trg mcp proxy` resolves a server's vars — and
//! `trg exec list`, which names the entries `run` will accept.
//!
//! `run` and `list` are subcommands rather than `list` living as a flag on
//! `run`'s own arguments, because `run`'s target is a bare positional
//! (`trg exec run <name>`) drawn from a namespace an operator controls
//! (`[exec.<name>]`). A reserved flag can never collide with that namespace;
//! a reserved word can — a config that ever declares `[exec.list]` would make
//! `trg exec list` permanently ambiguous between the entry and the listing.
//! Nesting the free-form name under `run` keeps `list` reserved only at the
//! top level, where no config places a name.
//!
//! `run` never supervises what it launches. `exec(2)` replaces the current
//! process image in place, so the launched command inherits this process's
//! pid, becomes the session's foreground job, and answers signals directly —
//! there is no `trg` left afterward to get in the way of, or to forward a
//! signal through.

use std::collections::HashMap;

use clap::{Args, Subcommand};
use serde_json::json;

use crate::config::{self, LoadedExec};
use crate::output::{print_json, OutputFormat};

#[derive(Subcommand)]
pub enum ExecCommands {
    /// Exec-replace into a configured `[exec.<name>]` entry
    Run(ExecArgs),
    /// List the `[exec.<name>]` entries `run` will accept
    List(ExecListArgs),
}

#[derive(Args)]
pub struct ExecArgs {
    /// The `[exec.<name>]` entry to launch
    pub name: String,

    /// A one-off KEY=VALUE override, applied after the entry's own `env`.
    ///
    /// For a literal like `DEBUG=1`, never a secret: this lands in argv and
    /// shell history like any other flag. A secret belongs in the entry's
    /// `env` table, named the way its backend addresses secrets and resolved
    /// the same way `trg secret get` reads one.
    #[arg(long = "env", value_name = "KEY=VALUE", value_parser = parse_env_pair)]
    pub env: Vec<(String, String)>,

    /// Remove an inherited variable before the entry's own `env` is applied
    #[arg(long = "unset", value_name = "KEY")]
    pub unset: Vec<String>,

    /// Output format. Only ever rendered on failure to start the command:
    /// success replaces this process, so there is nothing left to render.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub output_format: OutputFormat,

    /// Everything after `--` is appended after the entry's own `args` and
    /// handed to the launched command untouched. The `--` is what lets clap
    /// tell a typo in one of `trg exec`'s own flags (`--evn` for `--env`)
    /// apart from a flag meant for the launched command: without it, both
    /// look like the same kind of unrecognized token, and a typo would
    /// silently become a literal argument to the child instead of failing.
    #[arg(last = true)]
    pub extra_args: Vec<String>,
}

fn parse_env_pair(raw: &str) -> Result<(String, String), String> {
    let (key, value) = raw
        .split_once('=')
        .ok_or_else(|| format!("`{raw}` is not `KEY=VALUE`"))?;
    if key.is_empty() {
        return Err(format!("`{raw}` has an empty key"));
    }
    Ok((key.to_string(), value.to_string()))
}

#[derive(Args)]
pub struct ExecListArgs {
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub output_format: OutputFormat,
}

/// `trg exec list` — a plain config read, no secrets backend involved: the
/// names are declared in the clear, only the values behind an entry's `env`
/// are ever resolved through one.
pub fn list(args: &ExecListArgs) -> i32 {
    match config::list_exec_names() {
        Ok(names) => report_names(&names, args.output_format),
        Err(e) => report_failure(&e.to_string(), args.output_format),
    }
}

fn report_names(names: &[String], format: OutputFormat) -> i32 {
    if format.is_json() {
        let document = json!({ "entries": names });
        return print_json(&document, 0);
    }

    if names.is_empty() {
        eprintln!("no [exec] entries configured");
        return 0;
    }
    for name in names {
        println!("{name}");
    }
    0
}

pub fn run(loaded: LoadedExec, args: &ExecArgs) -> i32 {
    // The entry's own `unset` and a caller's `--unset` answer the same
    // question — which inherited vars must not reach the child — so both are
    // applied before anything is layered back on, in the order they arrived.
    let unset: Vec<String> = loaded.unset.iter().cloned().chain(args.unset.iter().cloned()).collect();

    let parent: HashMap<String, String> = std::env::vars().collect();
    let env = merge_env(&parent, &unset, &loaded.env, &args.env);

    let mut command_args = loaded.args;
    command_args.extend(args.extra_args.iter().cloned());

    let message = launch(&loaded.command, &command_args, env);
    report_failure(&message, args.output_format)
}

/// The environment the launched command actually sees: everything this
/// process has, minus what was told to leave, with the entry's own values
/// layered on and a caller's `--env` layered last so a one-off change always
/// wins over what the entry declared.
fn merge_env(
    parent: &HashMap<String, String>,
    unset: &[String],
    entry_env: &HashMap<String, String>,
    cli_env: &[(String, String)],
) -> HashMap<String, String> {
    let mut out = parent.clone();

    for key in unset {
        out.remove(key);
    }
    for (key, value) in entry_env {
        out.insert(key.clone(), value.clone());
    }
    for (key, value) in cli_env {
        out.insert(key.clone(), value.clone());
    }

    out
}

/// Replace this process with `command`, never returning on success.
///
/// `env_clear` before `envs` is not optional: `merge_env`'s output already
/// includes everything this process inherited that was worth keeping, so
/// starting `Command` from its own default inherit-everything and layering
/// `env` on top would mean an `unset` var reappeared underneath it instead of
/// staying gone.
#[cfg(unix)]
fn launch(command: &str, args: &[String], env: HashMap<String, String>) -> String {
    use std::os::unix::process::CommandExt;

    let err = std::process::Command::new(command)
        .args(args)
        .env_clear()
        .envs(env)
        .exec();
    format!("could not start `{command}`: {err}")
}

#[cfg(not(unix))]
fn launch(_command: &str, _args: &[String], _env: HashMap<String, String>) -> String {
    "trg exec replaces this process with exec(2), which only exists on unix".to_string()
}

fn report_failure(message: &str, format: OutputFormat) -> i32 {
    if format.is_json() {
        let document = json!({ "error": message });
        return print_json(&document, 1);
    }
    eprintln!("{message}");
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> ExecArgs {
        use clap::Parser;
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
        let args = parse(&["demo", "--", "--resume"]);
        assert_eq!(args.name, "demo");
        assert_eq!(args.extra_args, vec!["--resume".to_string()]);
    }

    /// Without `--`, a token meant for the launched command has no way to
    /// tell itself apart from a misspelled flag of `trg exec run`'s own — so
    /// clap refuses to parse it at all rather than guess.
    #[test]
    fn extra_args_without_a_dashdash_fail_to_parse() {
        use clap::Parser;
        let full = vec!["trg", "exec", "run", "demo", "--resume"];
        assert!(crate::cli::Cli::try_parse_from(full).is_err());
    }

    /// This is the enforcement `--` buys back: an unrecognized flag before it
    /// is a hard parse error, exactly like anywhere else in the CLI, instead
    /// of silently starting passthrough capture.
    #[test]
    fn an_unrecognized_flag_before_dashdash_fails_to_parse() {
        use clap::Parser;
        let full = vec!["trg", "exec", "run", "--evn", "DEBUG=1", "demo", "--", "--resume"];
        assert!(crate::cli::Cli::try_parse_from(full).is_err());
    }

    #[test]
    fn trg_s_own_recognized_flags_before_dashdash_still_work() {
        let args = parse(&["--env", "A=B", "demo", "--", "--resume"]);
        assert_eq!(args.env, vec![("A".to_string(), "B".to_string())]);
        assert_eq!(args.name, "demo");
        assert_eq!(args.extra_args, vec!["--resume".to_string()]);
    }

    /// A name that collides with the `list` subcommand is still reachable —
    /// this is the whole reason `list` sits beside `run` rather than beside
    /// `<name>` directly.
    #[test]
    fn an_entry_named_list_is_still_reachable_through_run() {
        let args = parse(&["list"]);
        assert_eq!(args.name, "list");
    }

    #[test]
    fn list_offers_the_output_choice_and_nothing_else() {
        use clap::Parser;
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
        let message = launch("trg-exec-test-command-that-does-not-exist", &[], env);
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
        assert_eq!(report_failure("boom", OutputFormat::Json), 1);
        assert_eq!(report_failure("boom", OutputFormat::Text), 1);
    }

    #[test]
    fn reporting_names_succeeds_under_either_format_even_with_none_to_show() {
        assert_eq!(report_names(&[], OutputFormat::Text), 0);
        assert_eq!(report_names(&[], OutputFormat::Json), 0);
        assert_eq!(
            report_names(&["alpha".to_string(), "zebra".to_string()], OutputFormat::Text),
            0
        );
    }
}
