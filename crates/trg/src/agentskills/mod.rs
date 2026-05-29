pub mod benchmark;
pub mod cache;
pub mod ci;
pub mod compare;
pub mod errors;
pub mod eval_suite_drift;
pub mod evals;
pub mod feedback;
pub mod grading;
pub mod improvement_bundle;
pub mod iteration_summary;
pub mod layout;
pub mod models;
pub mod outputs;
pub mod parser;
pub mod prompt;
pub mod redact;
pub mod report;
pub mod runner;
pub mod schemas;
pub mod validation;
pub mod validator;

pub(crate) fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write;
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(&mut out, "{b:02x}").expect("writing to String never fails");
    }
    out
}
