#![cfg_attr(
    all(dylint_lib = "trogon_dylint_lints", test),
    expect(
        prefer_integration_tests,
        reason = "The in-crate suites predate this rule and exercise exported modules through `use super::*`; moving them into tests/ needs a dedicated migration"
    )
)]
#![cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        acyclic_modules,
        reason = "Configuration and secret resolution share address types, while telemetry reconstructs runner records; these existing owner-level edges require a separate shared-model extraction"
    )
)]
#[cfg(not(unix))]
compile_error!("trg only supports Unix targets (Linux, macOS). On Windows, build and run inside WSL.");

#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod agentskills;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod cli;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod commands;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod config;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod exit_code;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod fs;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod oauth;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod output;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod secrets;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod shell;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod telemetry;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod term;
