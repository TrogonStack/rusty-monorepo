// edition:2024
// compile-flags: --test

#[path = "auxiliary/tmn_owner.rs"]
mod owner;

#[path = "auxiliary/layout_directory/mod.rs"]
mod directory;

#[path = "auxiliary/layout_generated.rs"]
mod generated;

#[path = "auxiliary/layout_generated_do_not_edit.rs"]
mod generated_banner;

#[cfg(any())]
#[path = "missing_tests.rs"]
mod feature_tests;

macro_rules! generated_module {
    () => {
        mod macro_tests {
            #[test]
            fn macro_test() {}
        }
    };
}

generated_module!();
