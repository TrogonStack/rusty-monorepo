#[cfg(not(unix))]
compile_error!("trg only supports Unix targets (Linux, macOS). On Windows, build and run inside WSL.");

pub mod agentskills;
pub mod commands;
pub mod config;
pub mod fs;
pub mod oauth;
pub mod telemetry;
