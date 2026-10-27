// edition:2024
// compile-flags: --test
#![allow(test_module_naming, reason = "this fixture isolates discarded error sources")]
#![allow(unused, dead_code)]

use std::convert::Infallible;
use std::num::ParseIntError;

enum AppError {
    Invalid,
    Parse(ParseIntError),
    Message(String),
}

fn parse(value: &str) -> Result<u32, ParseIntError> {
    value.parse()
}

// A wildcard parameter: fires.
fn wildcard(value: &str) -> Result<u32, AppError> {
    parse(value).map_err(|_| AppError::Invalid)
}

// A binding the body never reads: fires.
fn unused_binding(value: &str) -> Result<u32, AppError> {
    parse(value).map_err(|error| AppError::Invalid)
}

// An underscore-prefixed binding is still never read: fires.
fn underscore_binding(value: &str) -> Result<u32, AppError> {
    parse(value).map_err(|_error| AppError::Invalid)
}

// A block body that ignores the parameter: fires.
fn block_body(value: &str) -> Result<u32, AppError> {
    parse(value).map_err(|_| {
        let fallback = AppError::Invalid;
        fallback
    })
}

// `.ok()` straight into `ok_or`: fires.
fn ok_then_ok_or(value: &str) -> Result<u32, AppError> {
    parse(value).ok().ok_or(AppError::Invalid)
}

// `.ok()` straight into `ok_or_else`: fires.
fn ok_then_ok_or_else(value: &str) -> Result<u32, AppError> {
    parse(value).ok().ok_or_else(|| AppError::Invalid)
}

// The error is handed to a variant: fine.
fn variant_path(value: &str) -> Result<u32, AppError> {
    parse(value).map_err(AppError::Parse)
}

// The error is wrapped by a closure: fine.
fn variant_closure(value: &str) -> Result<u32, AppError> {
    parse(value).map_err(|error| AppError::Parse(error))
}

// The error is read before being replaced: fine.
fn read_before_replacing(value: &str) -> Result<u32, AppError> {
    parse(value).map_err(|error| {
        let _ = &error;
        AppError::Invalid
    })
}

// Stringifying reads the error: out of scope.
fn stringified(value: &str) -> Result<u32, AppError> {
    parse(value).map_err(|error| AppError::Message(error.to_string()))
}

// Fallbacks are not re-wrapping: out of scope.
fn fallbacks(value: &str) -> u32 {
    let first = parse(value).unwrap_or_else(|_| 0);
    let second = parse(value).or_else(|_| parse("0"));
    first + second.unwrap_or(0)
}

// `Option::ok_or` without a `Result::ok` before it carries no error: fine.
fn plain_option(value: Option<u32>) -> Result<u32, AppError> {
    value.ok_or(AppError::Invalid)
}

// A unit error has nothing to lose: fine.
fn unit_error(value: Result<u32, ()>) -> Result<u32, AppError> {
    value.map_err(|_| AppError::Invalid)
}

// `Infallible` has no value to keep: fine.
fn infallible_error(value: Result<u32, Infallible>) -> Result<u32, AppError> {
    value.map_err(|_| AppError::Invalid)
}

// `.ok()` on a unit error has nothing to lose: fine.
fn unit_ok(value: Result<u32, ()>) -> Result<u32, AppError> {
    value.ok().ok_or(AppError::Invalid)
}

// A macro expansion is not the author's choice: fine.
macro_rules! discard {
    ($result:expr) => {
        $result.map_err(|_| AppError::Invalid)
    };
}

fn from_macro(value: &str) -> Result<u32, AppError> {
    discard!(parse(value))
}

// A deliberate drop is stated with `expect`: fine.
#[expect(discarded_error_source, reason = "fixture for the documented opt-out")]
fn deliberate(value: &str) -> Result<u32, AppError> {
    parse(value).map_err(|_| AppError::Invalid)
}

// Test code is exempt: fine.
#[test]
fn test_code_is_exempt() {
    let _ = parse("1").map_err(|_| AppError::Invalid);
}

fn main() {}
