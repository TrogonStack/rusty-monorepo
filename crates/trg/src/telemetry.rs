//! File-based `tracing` subscriber shared by every `trg` subcommand.
//!
//! `trg mcp proxy` runs as a headless child of an MCP host (Cursor, Claude
//! Code) which swallows stdout and stderr. Without a file sink, rmcp's
//! internal `tracing::debug!`/`warn!` events — OAuth refresh attempts,
//! AS metadata fetches, 401 retries — are invisible when something breaks.
//! Other subcommands run interactively, but a single shared subscriber keeps
//! the wiring obvious and makes future cross-command diagnostics trivial.
//!
//! Default filter is `info,trg=debug,rmcp=debug` so refresh failures show up
//! without extra configuration; `RUST_LOG` overrides if you want more.

use std::{fs::OpenOptions, path::PathBuf, sync::Mutex};

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialise the global `tracing` subscriber, writing to
/// `$XDG_CACHE_HOME/trg/trg.log` (or `~/.cache/trg/trg.log`).
///
/// Idempotent: safe to call from multiple entry points; only the first call
/// succeeds (subsequent calls become no-ops via `try_init`). Silently does
/// nothing if neither `$XDG_CACHE_HOME` nor `$HOME` is set, or if the file
/// can't be opened — diagnostic logging must never crash the CLI.
pub fn init() {
    let Some(path) = log_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let writer = Mutex::new(file);
    let filter = EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| EnvFilter::new("info,trg=debug,rmcp=debug"));
    let layer = fmt::layer().with_writer(writer).with_ansi(false).with_target(true);
    let _ = tracing_subscriber::registry().with(filter).with(layer).try_init();
}

fn log_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("trg").join("trg.log"))
}
