// edition:2024
// compile-flags: --test

// A suite that fits in one file, loaded from the owner's sibling test file.
#[path = "auxiliary/tmn_owner.rs"]
mod owner;

// A module that holds tests but is misnamed: this is the violation.
#[path = "auxiliary/tmn_helpers.rs"]
mod helpers;

// A test-support module (no `#[test]` functions): must NOT be linted even
// though it is not named `tests`/`*_tests`.
#[path = "auxiliary/tmn_mocks.rs"]
mod mocks;
