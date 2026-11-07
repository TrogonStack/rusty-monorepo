// edition:2024
#![allow(unused, test_module_naming, reason = "a test-looking filename alone must not classify a normal target as test code")]

fn production_mutation() {
    unsafe { std::env::set_var("LINT_FIXTURE_KEY", "fixture") };
    unsafe { std::env::remove_var("LINT_FIXTURE_KEY") };
    let _ = std::env::set_current_dir(".");
}

fn main() {}
