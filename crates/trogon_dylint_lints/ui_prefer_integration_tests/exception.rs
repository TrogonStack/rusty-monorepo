// edition:2024
// compile-flags: --test --crate-type=lib
#![deny(unfulfilled_lint_expectations)]
#![allow(test_module_naming, inline_module_block, reason = "this fixture checks explicit placement exceptions")]

pub fn exported() -> bool { true }

#[cfg(test)]
mod tests {
    #[test]
    #[expect(prefer_integration_tests, reason = "this case intentionally exercises the unit-test configuration of the public API")]
    fn explicit_exception() {
        assert!(super::exported());
    }
}
