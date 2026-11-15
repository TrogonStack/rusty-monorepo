// edition:2024
// rustc-env:CARGO_TARGET_TMPDIR=fixture
#![allow(unused, reason = "this fixture models a Cargo integration target")]

fn integration_helper() {
    unsafe { std::env::remove_var("LINT_FIXTURE_KEY") };
}

fn main() {}
