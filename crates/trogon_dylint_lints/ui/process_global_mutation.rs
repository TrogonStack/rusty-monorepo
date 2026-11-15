// edition:2024
// compile-flags: --test
#![allow(unused, test_module_naming, inline_module_block, reason = "this fixture isolates process-global mutations")]

use std::env::{remove_var as remove_variable, set_current_dir as change_directory, set_var as set_variable};

#[test]
fn direct_mutations() {
    unsafe { std::env::set_var("LINT_FIXTURE_KEY", "fixture") };
    unsafe { std::env::remove_var("LINT_FIXTURE_KEY") };
    let _ = std::env::set_current_dir(".");
}

#[test]
fn imported_mutations() {
    unsafe { set_variable("LINT_FIXTURE_KEY", "fixture") };
    unsafe { remove_variable("LINT_FIXTURE_KEY") };
    let _ = change_directory(".");
}

fn production_function_is_not_test_code() {
    unsafe { std::env::set_var("LINT_FIXTURE_KEY", "fixture") };
    let _ = std::env::set_current_dir(".");
}

mod test_support {
    pub fn mutation_helper() {
        unsafe { std::env::remove_var("LINT_FIXTURE_KEY") };
    }
}

mod unrelated {
    pub fn set_var(_: &str, _: &str) {}
    pub fn remove_var(_: &str) {}
    pub fn set_current_dir(_: &str) {}
}

#[test]
fn lookalikes_are_not_process_mutations() {
    unrelated::set_var("LINT_FIXTURE_KEY", "fixture");
    unrelated::remove_var("LINT_FIXTURE_KEY");
    unrelated::set_current_dir(".");
}

#[test]
#[expect(process_global_mutation_in_tests, reason = "this test would run in its own isolated subprocess")]
fn deliberately_isolated() {
    unsafe { std::env::remove_var("LINT_FIXTURE_KEY") };
}

fn main() {}
