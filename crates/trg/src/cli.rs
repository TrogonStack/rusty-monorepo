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

    /// Shell lines, with a trailing `\` folded into the line it continues.
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

            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                let start = i + 1;
                let mut joined = String::new();
                loop {
                    let line = lines[i].trim();
                    match line.strip_suffix('\\') {
                        Some(head) if i + 1 < lines.len() => {
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
                out.push((start, joined));
            }
            i += 1;
        }
        out
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
            checked >= 20,
            "expected the docs to still hold `trg` invocations, found {checked}"
        );
    }
}
