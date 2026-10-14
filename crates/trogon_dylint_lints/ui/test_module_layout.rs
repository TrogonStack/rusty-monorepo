// edition:2024
// compile-flags: --test

#[path = "auxiliary/layout_wrong.rs"]
mod tests;

#[path = "auxiliary/layout_nested_owner.rs"]
mod nested_owner;

#[path = "auxiliary/layout_inline_directory/mod.rs"]
mod inline_directory;

#[path = "auxiliary/layout_group_directory/mod.rs"]
mod group_directory;

#[path = "auxiliary/layout_inline_group_owner.rs"]
mod inline_group_owner;

fn declares_local_tests() {
    #[path = "auxiliary/layout_wrong.rs"]
    mod tests;
}
