// edition:2024
#![allow(unused, dead_code)]

// With `avoid_breaking_exported_api = false`, a `pub` type's `new` is no
// longer exempt: fires.
pub struct PublicId(String);

impl PublicId {
    pub fn new(value: String) -> Self {
        Self(value)
    }
}

fn main() {}
