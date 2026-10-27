// edition:2024
#![allow(unused)]

// Public fields, no `Default`: fires.
pub struct ConnectOptions {
    pub retries: u32,
    pub verbose: bool,
}

// `pub(crate)` counts as public: fires.
pub struct WriteOptions {
    pub(crate) sync: bool,
    pub level: u8,
}

// `pub(super)` and `pub(crate)` both count as public: fires.
#[allow(inline_module_block)]
mod nested {
    pub struct FlushOptions {
        pub(super) sync: bool,
        pub(crate) level: u8,
    }
}

// A lifetime parameter does not change whether `Default` is implemented: fires.
pub struct ReadOptions<'a> {
    pub label: &'a str,
}

// Derived `Default`: must NOT fire.
#[derive(Default)]
pub struct DerivedOptions {
    pub retries: u32,
}

// Hand-written `Default`: must NOT fire.
pub struct ManualOptions {
    pub retries: u32,
}

impl Default for ManualOptions {
    fn default() -> Self {
        Self { retries: 3 }
    }
}

// A private field makes it more than a bag of options: must NOT fire.
pub struct PrivateOptions {
    pub retries: u32,
    verbose: bool,
}

// The name does not end in `Options`: must NOT fire.
pub struct Settings {
    pub retries: u32,
}

// A tuple struct has no field names to update: must NOT fire.
pub struct TupleOptions(pub u32);

// A unit struct has nothing to set: must NOT fire.
pub struct UnitOptions;

// A struct without fields has nothing to set: must NOT fire.
pub struct EmptyOptions {}

// Generic structs are skipped: must NOT fire.
pub struct GenericOptions<T> {
    pub value: T,
}

// Explicitly allowed at the site: must NOT fire.
#[allow(options_struct_without_default)]
pub struct AllowedOptions {
    pub retries: u32,
}

fn main() {}
