// edition:2024
#![feature(print_internals, slice_internals)]
#![allow(unused, dead_code, internal_features)]

// With `owned_crates = ["std", "core"]`, their hidden items are allowed.
fn owned() -> Option<usize> {
    std::io::_print(format_args!("owned\n"));
    core::slice::memchr::memchr(1, &[1, 2, 3])
}

fn main() {}
