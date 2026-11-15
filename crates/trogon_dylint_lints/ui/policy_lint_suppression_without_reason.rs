// edition:2024
#![allow(unused, reason = "this fixture isolates policy suppression reasons")]

#[allow(function_local_use, reason = "")]
fn empty_reason() {}

#[allow(unbounded_channel, reason = " \t ")]
fn whitespace_reason() {}

#[cfg_attr(dylint_lib = "trogon_dylint_lints", allow(inline_module_block, reason = " "))]
fn conditional_blank_reason() {}

#[cfg_attr(dylint_lib = "trogon_dylint_lints", allow(function_local_use))]
fn conditional_missing_reason() {}

#[allow(function_local_use, reason = "this fixture validates a documented suppression")]
fn documented_reason() {}

#[allow(function_local_use)]
fn missing_reason() {}

#[expect(function_local_use, reason = " \t ")]
fn blank_expectation() {
    use std::mem;
    let _ = mem::size_of::<u8>();
}

#[expect(function_local_use)]
fn missing_expectation() {
    use std::mem;
    let _ = mem::size_of::<u8>();
}

#[expect(function_local_use, reason = "this fixture validates a documented expectation")]
fn documented_expectation() {
    use std::mem;
    let _ = mem::size_of::<u8>();
}

#[allow(dead_code, reason = "")]
fn unrelated_lint() {}

fn main() {}
