// edition:2024
// compile-flags: --test

#[test]
fn root_test() {}

// An attribute macro re-emits the item from expansion but keeps the author's
// name, so a test declared through one is still the author's placement error.
macro_rules! root_test {
    ($name:ident) => {
        #[test]
        fn $name() {}
    };
}

root_test!(expanded_root_test);

// A generator that names the test itself is exempt, like generated modules.
macro_rules! generated_root_test {
    () => {
        #[test]
        fn generated_root_test() {}
    };
}

generated_root_test!();
