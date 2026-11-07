// edition:2024
#![allow(unused, reason = "this fixture isolates error source preservation")]
#![deny(unfulfilled_lint_expectations)]

use std::fmt;
use std::num::ParseIntError;

enum AppError {
    Message(String),
    StructMessage { message: String },
    MessageAndSource(String, ParseIntError),
    Wrapped { message: String, source: ParseIntError },
}

#[expect(discarded_error_source, reason = "the typed cause is replaced by its display string")]
fn direct(value: Result<u32, ParseIntError>) -> Result<u32, String> {
    value.map_err(|error| error.to_string())
}

#[expect(discarded_error_source, reason = "the tuple variant retains only display text")]
fn tuple_variant(value: Result<u32, ParseIntError>) -> Result<u32, AppError> {
    value.map_err(|error| AppError::Message(error.to_string()))
}

#[expect(discarded_error_source, reason = "the trait call also replaces the typed cause")]
fn trait_call(value: Result<u32, ParseIntError>) -> Result<u32, String> {
    value.map_err(|error| ToString::to_string(&error))
}

#[expect(discarded_error_source, reason = "the struct variant retains only display text")]
fn struct_variant(value: Result<u32, ParseIntError>) -> Result<u32, AppError> {
    value.map_err(|error| AppError::StructMessage { message: error.to_string() })
}

fn preserved_source(value: Result<u32, ParseIntError>) -> Result<u32, AppError> {
    value.map_err(|error| AppError::Wrapped {
        message: error.to_string(),
        source: error,
    })
}

fn preserved_tuple_source(value: Result<u32, ParseIntError>) -> Result<u32, AppError> {
    value.map_err(|error| AppError::MessageAndSource(error.to_string(), error))
}

fn string_source(value: Result<u32, String>) -> Result<u32, AppError> {
    value.map_err(|error| AppError::Message(error.to_string()))
}

#[derive(Debug)]
struct DomainError;

impl DomainError {
    fn to_string(self) -> Self {
        self
    }
}

impl fmt::Display for DomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("domain error")
    }
}

#[allow(manual_error_impl, reason = "this fixture needs a dependency-free Error implementation")]
impl std::error::Error for DomainError {}

fn unrelated_method(value: Result<u32, DomainError>) -> Result<u32, DomainError> {
    value.map_err(|error| error.to_string())
}

fn main() {}
