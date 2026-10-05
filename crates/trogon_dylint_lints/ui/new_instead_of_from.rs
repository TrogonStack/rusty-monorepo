// edition:2024
#![allow(unused, dead_code)]

use std::borrow::Cow;

// A tuple struct whose `new` only wraps its argument: fires.
struct UserId(String);

impl UserId {
    fn new(value: String) -> Self {
        Self(value)
    }
}

// A named-field struct whose `new` only wraps its argument: fires.
struct Wrapper {
    value: String,
}

impl Wrapper {
    fn new(value: String) -> Self {
        Self { value: value }
    }
}

// The same, written with the field-init shorthand: fires.
struct ShorthandWrapper {
    value: String,
}

impl ShorthandWrapper {
    fn new(value: String) -> Self {
        Self { value }
    }
}

// `.into()` carrying the argument across a type change: fires.
struct Name(String);

impl Name {
    fn new(value: &str) -> Self {
        Self(value.into())
    }
}

// `.to_owned()` carrying the argument across a type change: fires.
struct Label(String);

impl Label {
    fn new(value: &str) -> Self {
        Self(value.to_owned())
    }
}

// `.to_string()` carrying the argument across a type change: fires.
struct Count(String);

impl Count {
    fn new(value: u32) -> Self {
        Self(value.to_string())
    }
}

// `.into_owned()` carrying the argument across a type change: fires.
struct Owned(String);

impl Owned {
    fn new(value: Cow<'_, str>) -> Self {
        Self(value.into_owned())
    }
}

// Also written using the type's own name instead of `Self`: fires.
struct Spelled(String);

impl Spelled {
    fn new(value: String) -> Spelled {
        Spelled(value)
    }
}

// `impl Into<T>` would conflict with the suggested `From` impl (E0119):
// must NOT fire.
struct Accepting(String);

impl Accepting {
    fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

// A generic parameter on the function has the same E0119 conflict:
// must NOT fire.
struct Generic(String);

impl Generic {
    fn new<T: Into<String>>(value: T) -> Self {
        Self(value.into())
    }
}

// Two parameters is not a plain wrap: must NOT fire.
struct Pair(String);

impl Pair {
    fn new(value: String, _unused: u8) -> Self {
        Self(value)
    }
}

// No parameters is not a plain wrap either: must NOT fire.
struct Empty(Vec<String>);

impl Empty {
    fn new() -> Self {
        Self(Vec::new())
    }
}

// A multi-field struct has nowhere for `From` to put a second field:
// must NOT fire.
struct Multi {
    value: String,
    flag: bool,
}

impl Multi {
    fn new(value: String) -> Self {
        Self { value, flag: false }
    }
}

// Returning `Result` means construction can fail, which is not what `From`
// promises: must NOT fire.
struct Checked(String);

impl Checked {
    fn new(value: String) -> Result<Self, String> {
        Ok(Self(value))
    }
}

// A `let` in the body means `new` does more than hand the argument off:
// must NOT fire.
struct Trimmed(String);

impl Trimmed {
    fn new(value: String) -> Self {
        let value = value.trim().to_owned();
        Self(value)
    }
}

// Setup work before construction means `new` earns its name: must NOT fire.
struct Connection(String);

impl Connection {
    fn new(endpoint: String) -> Self {
        validate(&endpoint);
        Self(endpoint)
    }
}

fn validate(_endpoint: &str) {}

trait Build {
    fn new(value: String) -> Self;
}

// The implementor owns neither the name nor the choice to expose `From`
// instead: must NOT fire.
struct Built(String);

impl Build for Built {
    fn new(value: String) -> Self {
        Self(value)
    }
}

// Explicitly allowed at the site: must NOT fire.
struct Allowed(String);

impl Allowed {
    #[allow(new_instead_of_from)]
    fn new(value: String) -> Self {
        Self(value)
    }
}

// The test-support module family is exempt: must NOT fire.
#[allow(inline_module_block)]
mod tests {
    pub struct Sample(String);

    impl Sample {
        pub fn new(value: String) -> Self {
            Self(value)
        }
    }
}

// Reachable from outside the crate, so the default config leaves it alone:
// must NOT fire.
pub struct PublicId(String);

impl PublicId {
    pub fn new(value: String) -> Self {
        Self(value)
    }
}

// Chained conversions do more than one hand-off: does not fire.
struct Chained(String);

impl Chained {
    fn new(value: &str) -> Self {
        Self(value.to_owned().into())
    }
}

// An `unsafe` constructor carries a contract `From` cannot express: does not fire.
struct Unchecked(u32);

impl Unchecked {
    unsafe fn new(value: u32) -> Self {
        Self(value)
    }
}

// A `const` constructor stays usable in const contexts, `From::from` does not: does not fire.
struct ConstWrapper(u32);

impl ConstWrapper {
    const fn new(value: u32) -> Self {
        Self(value)
    }
}

// The conversion already exists: does not fire.
struct AlreadyConverts(String);

impl From<&str> for AlreadyConverts {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl AlreadyConverts {
    fn new(value: &str) -> Self {
        Self(value.to_owned())
    }
}

// Core's identity `From<Recursive> for Recursive` already exists: does not fire.
struct Recursive(Box<Option<Recursive>>);

impl Recursive {
    fn new(value: Recursive) -> Self {
        Self(Box::new(Some(value)).into())
    }
}

struct Inner(String);

impl Inner {
    fn into(self) -> String {
        assert!(!self.0.is_empty());
        self.0
    }
}

// An inherent method that merely shares a conversion's name: does not fire.
struct Validated(String);

impl Validated {
    fn new(value: Inner) -> Self {
        Self(value.into())
    }
}

struct Specialized<T>(T);

// A concrete impl of a generic type is named with its arguments: fires.
impl Specialized<String> {
    fn new(value: String) -> Self {
        Self(value)
    }
}

fn main() {}
