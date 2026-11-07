#![cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        acyclic_modules,
        reason = "Evaluation reports retain grading and runner models that also produce those reports; untangling this existing data model is tracked separately"
    )
)]
use std::fmt::Write;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod arm_timing;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod benchmark;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod budget;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod cache;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod call_bounds;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod case_directories;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod case_env;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod case_selection;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod ci;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod companion_skills;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod compare;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod concurrency;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod dispersion;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod efficiency;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod errors;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod eval_suite_drift;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod eval_telemetry;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod evals;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod feedback;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod grader_agreement;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod graders;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod grading;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod headroom;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod html_report;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod improvement_bundle;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod iteration_summary;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod judge;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod judge_votes;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod layout;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod mcp_recording;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod mocks;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod model_family;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod model_name;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod models;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod outputs;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod parser;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod permission_outcome;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod prompt;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod proportion;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod real_mcp_server;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod redact;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod replay;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod report;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod runner;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod sampling;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod scaling;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod scenario_selection;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod schemas;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod shift;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod skill_trust;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod span_reference;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod system_prompt_appendix;
#[cfg(test)]
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
mod tests;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod tool_grant;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod transcript;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod transcript_metrics;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod validation;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod validator;
#[cfg_attr(dylint_lib = "trogon_dylint_lints", deny(acyclic_modules))]
pub mod workspace_scaffold;

pub(crate) fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(&mut out, "{b:02x}").expect("writing to String never fails");
    }
    out
}
