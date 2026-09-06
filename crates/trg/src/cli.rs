//! The `trg` command tree.
//!
//! Defined in the library rather than in `main.rs` so the parser the binary
//! uses is the same one tests can hand a command line to. Every `trg ...`
//! invocation printed in the docs is checked against it, which is only
//! meaningful if there is one definition rather than two.

use clap::Parser;

use crate::commands::Commands;

#[derive(Parser)]
#[command(name = "trg")]
#[command(version)]
#[command(about = "TrogonStack tools and utilities")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

/// Every `trg` invocation printed in the docs, handed to the real parser.
///
/// A flag that was renamed leaves the docs telling people to run something
/// clap now rejects, and nothing catches it: the example is prose to the
/// compiler and a command to the reader. Parsing is enough to find that,
/// and it runs nothing.
#[cfg(test)]
mod doc_commands {
    use std::path::{Path, PathBuf};

    use clap::Parser;

    use super::Cli;

    fn crate_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn markdown_files() -> Vec<PathBuf> {
        let root = crate_root();
        let mut out = vec![root.join("README.md")];
        collect(&root.join("docs"), &mut out);
        out.sort();
        out
    }

    fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect(&path, out);
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }

    const SHELL_FENCES: [&str; 4] = ["```sh", "```bash", "```shell", "```console"];

    /// Commands from the shell fences, with a trailing `\` folded into the line
    /// it continues.
    ///
    /// A fence that prompts with `$` is a transcript, so only the prompted
    /// lines are commands and the rest is output. A fence without a prompt is a
    /// script, where every line is.
    fn shell_lines(file: &Path) -> Vec<(usize, String)> {
        let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        let lines: Vec<&str> = text.lines().collect();

        let mut out = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let fence = lines[i].trim_start();
            if !SHELL_FENCES.contains(&fence) {
                i += 1;
                continue;
            }
            i += 1;

            let body_start = i;
            let mut end = i;
            while end < lines.len() && !lines[end].trim_start().starts_with("```") {
                end += 1;
            }
            let prompted = lines[body_start..end].iter().any(|l| is_prompt(l));

            while i < end {
                let start = i + 1;
                let is_command = !prompted || is_prompt(lines[i]);

                let mut joined = String::new();
                loop {
                    let line = strip_prompt(lines[i].trim());
                    match line.strip_suffix('\\') {
                        Some(head) if i + 1 < end => {
                            joined.push_str(head.trim_end());
                            joined.push(' ');
                            i += 1;
                        }
                        _ => {
                            joined.push_str(line);
                            i += 1;
                            break;
                        }
                    }
                }

                if is_command {
                    out.push((start, joined));
                }
            }
            i = end + 1;
        }
        out
    }

    fn is_prompt(line: &str) -> bool {
        let line = line.trim_start();
        line == "$" || line.starts_with("$ ")
    }

    fn strip_prompt(line: &str) -> &str {
        line.strip_prefix("$ ").map_or(line, str::trim_start)
    }

    /// Split on whitespace, honouring quotes, and drop what the shell would
    /// consume before the program ever sees it.
    fn tokenize(line: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut started = false;
        let mut quote: Option<char> = None;

        for c in line.chars() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), c) => cur.push(c),
                (None, '\'' | '"') => {
                    quote = Some(c);
                    started = true;
                }
                (None, c) if c.is_whitespace() => {
                    if started {
                        out.push(std::mem::take(&mut cur));
                        started = false;
                    }
                }
                (None, c) => {
                    cur.push(c);
                    started = true;
                }
            }
        }
        if started {
            out.push(cur);
        }
        out
    }

    /// The `trg ...` invocations in one shell line, one per pipeline stage.
    ///
    /// Still carrying their redirections, because a synopsis writes its
    /// placeholder as `<name>` and stripping redirections first would take that
    /// for one and leave a command that is missing an argument rather than one
    /// that was never a command.
    fn invocations(line: &str) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        for stage in split_pipeline(line) {
            let tokens = tokenize(&stage);
            if tokens.first().map(String::as_str) == Some("trg") {
                out.push(tokens);
            }
        }
        out
    }

    /// Pipeline stages, ignoring a `|` that is inside quotes.
    fn split_pipeline(line: &str) -> Vec<String> {
        let mut out = vec![String::new()];
        let mut quote: Option<char> = None;
        for c in line.chars() {
            match (quote, c) {
                (Some(q), c) if c == q => {
                    quote = None;
                    out.last_mut().expect("stage").push(c);
                }
                (None, '\'' | '"') => {
                    quote = Some(c);
                    out.last_mut().expect("stage").push(c);
                }
                (None, '|') => out.push(String::new()),
                (_, c) => out.last_mut().expect("stage").push(c),
            }
        }
        out
    }

    /// Redirections and the heredoc opener belong to the shell, not to clap.
    fn strip_redirections(tokens: Vec<String>) -> Vec<String> {
        let mut out = Vec::new();
        let mut skip_next = false;
        for token in tokens {
            if skip_next {
                skip_next = false;
                continue;
            }
            if token.starts_with("<<") || token.contains(">&") {
                continue;
            }
            if token == "<" || token == ">" || token == ">>" {
                skip_next = true;
                continue;
            }
            if token.starts_with('>') || token.starts_with('<') {
                continue;
            }
            out.push(token);
        }
        out
    }

    /// A synopsis stands in for an invocation with `<NAME>`, `[OPTIONS]`, or a
    /// bare `...`, and describes a shape rather than a command to run.
    fn is_synopsis(tokens: &[String]) -> bool {
        tokens
            .iter()
            .any(|t| t == "..." || (t.starts_with('<') && t.ends_with('>')) || (t.starts_with('[') && t.ends_with(']')))
    }

    #[test]
    fn every_documented_trg_command_still_parses() {
        let mut checked = 0usize;

        for file in markdown_files() {
            for (line_no, line) in shell_lines(&file) {
                for tokens in invocations(&line) {
                    if is_synopsis(&tokens) {
                        continue;
                    }
                    let tokens = strip_redirections(tokens);
                    checked += 1;

                    if let Err(e) = Cli::try_parse_from(&tokens) {
                        panic!(
                            "{}:{line_no}: this documented command no longer parses:\n  {}\n\n{e}",
                            file.display(),
                            tokens.join(" "),
                        );
                    }
                }
            }
        }

        assert!(
            checked >= 44,
            "expected the docs to still hold `trg` invocations, found {checked}"
        );
    }
}

/// One spelling of the output choice, everywhere it is offered.
///
/// Two commands that both render a result and disagree about how to ask for
/// the machine-readable one leave a caller reading `--help` for each of them.
/// Sharing [`OutputFormat`] gets the type right; only walking the built command
/// tree gets the flag right, since a command is free to declare its own.
#[cfg(test)]
mod output_format {
    use clap::{Command, CommandFactory};

    use super::Cli;

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

    /// The one command whose stdout is not a result to render.
    ///
    /// `proxy` speaks JSON-RPC on stdout to whatever launched it, so an
    /// `--output-format` there would offer a choice it cannot honour.
    const WITHOUT_AN_OUTPUT_CHOICE: [&str; 1] = ["trg mcp proxy"];

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
            for arg in cmd.get_arguments().filter(|a| a.get_long() == Some("output-format")) {
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
}
