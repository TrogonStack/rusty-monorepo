// edition:2024
#![feature(print_internals, slice_internals, fmt_helpers_for_derive)]
#![allow(unused, dead_code, internal_features)]

use core::slice::memchr;
use core::slice::memchr::memrchr;
use std::io::_print;

#[derive(Debug, Clone, PartialEq)]
struct Derived(u8);

fn hidden_function() {
    _print(format_args!("hidden\n"));
}

fn hidden_module() -> Option<usize> {
    core::slice::memchr::memchr(1, &[1, 2, 3])
}

struct ByMethod(u8);

impl std::fmt::Debug for ByMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple_field1_finish("ByMethod", &self.0)
    }
}

struct ByAssociatedFunction(u8);

impl std::fmt::Debug for ByAssociatedFunction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Formatter::debug_tuple_field1_finish(f, "ByAssociatedFunction", &self.0)
    }
}

fn macro_expansion() -> String {
    assert_eq!(Derived(1), Derived(1).clone());
    format!("{:?}", Derived(1))
}

fn public_item() -> Option<usize> {
    [1u8, 2, 3].iter().position(|&byte| byte == 1)
}

#[allow(doc_hidden_access)]
fn allowed() -> Option<usize> {
    core::slice::memchr::memchr(1, &[1, 2, 3])
}

fn main() {}
