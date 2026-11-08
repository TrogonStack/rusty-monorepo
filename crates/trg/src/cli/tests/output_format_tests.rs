use clap::{Command, CommandFactory};

use super::super::Cli;

/// Every subcommand, by the path a caller would type.
fn commands() -> Vec<(String, Command)> {
    let mut out = Vec::new();
    walk(&Cli::command(), "trg", &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn walk(cmd: &Command, path: &str, out: &mut Vec<(String, Command)>) {
    out.push((path.to_string(), cmd.clone()));
    for sub in cmd.get_subcommands() {
        walk(sub, &format!("{path} {}", sub.get_name()), out);
    }
}

fn long_flags(cmd: &Command) -> Vec<String> {
    cmd.get_arguments()
        .filter_map(|a| a.get_long())
        .map(String::from)
        .collect()
}

/// Every command a caller can actually run, ignoring the groupings that only
/// exist to reach one and the `help` subcommand clap writes itself.
fn runnable_commands() -> Vec<(String, Command)> {
    commands()
        .into_iter()
        .filter(|(path, cmd)| cmd.get_subcommands().next().is_none() && !path.ends_with(" help"))
        .collect()
}

/// Commands whose stdout is not a result to render.
///
/// `proxy` and `mock-server` both speak JSON-RPC on stdout to whatever launched them,
/// so an `--output-format` there would offer a choice neither can honour.
const WITHOUT_AN_OUTPUT_CHOICE: [&str; 2] = ["trg mcp proxy", "trg ai skills eval mock-server"];

/// Offered everywhere, stated as the rule rather than as a list of the
/// commands that happen to have it, so a command added tomorrow is held to
/// it without anyone remembering to add it here.
#[test]
fn every_command_offers_the_output_choice() {
    let missing: Vec<String> = runnable_commands()
        .into_iter()
        .filter(|(path, _)| !WITHOUT_AN_OUTPUT_CHOICE.contains(&path.as_str()))
        .filter(|(_, cmd)| !long_flags(cmd).iter().any(|f| f == "output-format"))
        .map(|(path, _)| path)
        .collect();

    assert!(
        missing.is_empty(),
        "these commands do not offer `--output-format`: {missing:?}"
    );
}

/// The exclusion earns its place by naming a command that exists, or it is
/// a typo that silently excuses nothing.
#[test]
fn the_excluded_command_still_exists() {
    let paths: Vec<String> = runnable_commands().into_iter().map(|(path, _)| path).collect();
    for excluded in WITHOUT_AN_OUTPUT_CHOICE {
        assert!(
            paths.contains(&excluded.to_string()),
            "`{excluded}` is excused from `--output-format` but is not a command"
        );
    }
}

/// `--output-format text` and `--output-format json`, defaulting to text,
/// and described in `--help` with the same placeholder, or it is not the
/// same flag however similarly it is spelled.
#[test]
fn every_output_choice_reads_the_same_way() {
    for (path, cmd) in commands() {
        let selected_items = cmd.get_arguments().filter(|a| a.get_long() == Some("output-format"));
        for arg in selected_items {
            let values: Vec<String> = arg
                .get_possible_values()
                .iter()
                .map(|v| v.get_name().to_string())
                .collect();
            assert_eq!(values, ["text", "json"], "{path}: --output-format takes other values");

            let defaults: Vec<String> = arg
                .get_default_values()
                .iter()
                .map(|v| v.to_string_lossy().into_owned())
                .collect();
            assert_eq!(defaults, ["text"], "{path}: --output-format defaults elsewhere");

            let placeholder = arg.get_value_names().map(<[_]>::to_vec).unwrap_or_default();
            assert_eq!(
                placeholder.iter().map(ToString::to_string).collect::<Vec<_>>(),
                ["OUTPUT_FORMAT"],
                "{path}: --output-format is described with another placeholder"
            );
        }
    }
}

/// The spellings this replaced. A command that reintroduces one splits the
/// convention again, which is the thing the shared type cannot prevent on
/// its own.
#[test]
fn no_command_uses_a_spelling_this_replaced() {
    for (path, cmd) in commands() {
        for stale in ["json", "format"] {
            assert!(
                !long_flags(&cmd).iter().any(|f| f == stale),
                "{path}: `--{stale}` is spelled `--output-format`"
            );
        }
    }
}
