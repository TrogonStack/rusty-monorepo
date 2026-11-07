// edition:2024
// compile-flags: --test
#![allow(dead_code)]
#![deny(unfulfilled_lint_expectations)]

use std::io;

#[expect(
    error_string_comparison,
    reason = "production routing must stay checked when the crate is built as a test target"
)]
fn production_routes_by_error_text(error: io::Error) -> bool {
    error.to_string().contains("unavailable")
}

#[allow(test_module_naming, reason = "the auxiliary fixture isolates diagnostic assertions from layout policy")]
#[cfg(test)]
#[path = "auxiliary/error_message_assertion_tests.rs"]
mod tests;

#[allow(test_module_naming, reason = "the root test isolates test-function context from module and file names")]
#[test]
fn root_test_may_check_rendered_diagnostics() {
    let error = io::Error::other("service unavailable");
    assert!(error.to_string().contains("unavailable"));
}
