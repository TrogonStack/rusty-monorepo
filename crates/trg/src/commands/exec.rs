//! `trg exec run` — exec-replace into any command with its `env` resolved
//! the same way `trg mcp proxy` resolves a server's vars — and
//! `trg exec list`, which names the entries `run` will accept.
//!
//! `run`'s target is named with `--name` (`trg exec run --name <name>`)
//! rather than read as a bare word, so an entry drawn from the namespace an
//! operator controls (`[exec.<name>]`) can never be mistaken for a
//! subcommand: a config that declares `[exec.list]` stays reachable.
//!
//! `run` never supervises what it launches. `exec(2)` replaces the current
//! process image in place, so the launched command inherits this process's
//! pid, becomes the session's foreground job, and answers signals directly —
//! there is no `trg` left afterward to get in the way of, or to forward a
//! signal through.

use std::collections::HashMap;
use std::os::unix::process::CommandExt;

use clap::{Args, Subcommand};
use serde::Serialize;

use crate::config::{self, LoadedExec};
use crate::exit_code::ExitCode;
use crate::output::{print_json, OutputFormat};
use crate::telemetry::semconv::trg::EXEC_ENV_VAR_COUNT;
use crate::telemetry::{propagation, Telemetry};

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
    #[arg(long)]
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
pub fn list(args: &ExecListArgs) -> ExitCode {
    match config::list_exec_names() {
        Ok(names) => report_names(&names, args.output_format),
        Err(e) => report_failure(&e.to_string(), args.output_format),
    }
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
fn report_names(names: &[String], format: OutputFormat) -> ExitCode {
    if format.is_json() {
        #[derive(Serialize)]
        struct ExecListDocument<'a> {
            entries: &'a [String],
        }
        let document = ExecListDocument { entries: names };
        return print_json(&document, ExitCode::Success);
    }

    if names.is_empty() {
        eprintln!("no [exec] entries configured");
        return ExitCode::Success;
    }
    for name in names {
        println!("{name}");
    }
    ExitCode::Success
}

/// Resolves everything the launched command needs, including the trace
/// context it continues, which is why this runs inside the root span. The
/// `exec(2)` itself is left to [`PreparedLaunch::exec`], outside it.
pub fn run(loaded: LoadedExec, args: &ExecArgs) -> PreparedLaunch {
    // The entry's own `unset` and a caller's `--unset` answer the same
    // question — which inherited vars must not reach the child — so both are
    // applied before anything is layered back on, in the order they arrived.
    let unset: Vec<String> = loaded.unset.iter().cloned().chain(args.unset.iter().cloned()).collect();

    let parent: HashMap<String, String> = std::env::vars().collect();
    let env = traced_merge_env(&parent, &unset, &loaded.env, &args.env);

    let mut command_args = loaded.args;
    command_args.extend(args.extra_args.iter().cloned());

    PreparedLaunch::new(loaded.command, command_args, env, args.output_format)
}

/// [`merge_env`] under a span recording how many variables the launched
/// command receives, and never which ones or what they hold: the entry's own
/// `env` is where its resolved secrets live.
fn traced_merge_env(
    parent: &HashMap<String, String>,
    unset: &[String],
    entry_env: &HashMap<String, String>,
    cli_env: &[(String, String)],
) -> HashMap<String, String> {
    let span = tracing::info_span!("merge env", { EXEC_ENV_VAR_COUNT } = tracing::field::Empty);
    span.in_scope(|| {
        let env = merge_env(parent, unset, entry_env, cli_env);
        span.record(EXEC_ENV_VAR_COUNT, i64::try_from(env.len()).unwrap_or(i64::MAX));
        env
    })
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

/// A command ready to replace this process, built while the root span was
/// current so its environment carries that span as the parent.
///
/// The replacement happens outside the root span on purpose: the span only
/// exports once it ends, and nothing runs in this process after a
/// successful `exec(2)` to end it.
pub struct PreparedLaunch {
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
    output_format: OutputFormat,
}

impl PreparedLaunch {
    /// The trace context is injected into `env` directly rather than
    /// through `propagation::inject_std_command`, since the launched
    /// `Command` clears its environment and the carrier pairs must survive
    /// that.
    fn new(command: String, args: Vec<String>, mut env: HashMap<String, String>, output_format: OutputFormat) -> Self {
        env.extend(propagation::carrier_pairs());
        Self {
            command,
            args,
            env,
            output_format,
        }
    }

    /// Ends `telemetry`, then replaces this process with the command, never
    /// returning on success. On failure, reports why and returns the exit
    /// code to leave with.
    pub fn exec(self, telemetry: Telemetry) -> ExitCode {
        self.exec_with(telemetry, |command| command.exec())
    }

    /// `env_clear` before `envs` is not optional: `merge_env`'s output
    /// already includes everything this process inherited that was worth
    /// keeping, so starting `Command` from its own default
    /// inherit-everything and layering `env` on top would mean an `unset`
    /// var reappeared underneath it instead of staying gone.
    fn exec_with(
        self,
        telemetry: Telemetry,
        replace: impl FnOnce(&mut std::process::Command) -> std::io::Error,
    ) -> ExitCode {
        telemetry.end_before_exec();
        let message = self.replace_with(replace);
        report_failure(&message, self.output_format)
    }

    /// Returns only when `replace` failed, with a message naming the command
    /// and never its environment, which holds the entry's resolved secrets.
    fn replace_with(&self, replace: impl FnOnce(&mut std::process::Command) -> std::io::Error) -> String {
        let mut command = std::process::Command::new(&self.command);
        command.args(&self.args).env_clear().envs(&self.env);
        let err = replace(&mut command);
        format!("could not start `{}`: {err}", self.command)
    }
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
fn report_failure(message: &str, format: OutputFormat) -> ExitCode {
    if format.is_json() {
        #[derive(Serialize)]
        struct ExecFailureDocument<'a> {
            error: &'a str,
        }
        let document = ExecFailureDocument { error: message };
        return print_json(&document, ExitCode::Failed);
    }
    eprintln!("{message}");
    ExitCode::Failed
}

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;
