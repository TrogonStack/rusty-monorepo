//! Compile-time literal constructors behind the `trogon_claims` macros.
//!
//! This crate holds the procedural half of `object_id!`, `object_type!`,
//! `principal_kind!`, `relation_name!` and `param_name!`. Each `*_impl` macro
//! validates a string literal with the rules in `trogon_claims_syntax` while the
//! compiler expands it, so an invalid literal is a compile error pointing at the
//! literal. It has no `syn` or `quote` dependency.
//!
//! **Do not depend on this crate directly.** The `*_impl` macros take a crate
//! path as their first argument and are meant to be called only through the
//! declarative macros that `trogon_claims` exports, which supply it. Raw string
//! literals are accepted; literals with escape sequences are refused, because no
//! valid value needs one.
//!
//! Consumers enable `trogon_claims`'s opt-in `macros` feature to use these public
//! helpers. `object_id!`, `object_type!`, `relation_name!` and `param_name!`
//! additionally require `spicedb`, which forwards this crate's default-off
//! `spicedb` feature. Without it, only `principal_kind_impl` is exported.
//! Runtime types and validated constructors do not require the `macros` feature.
//!
//! The crate is not published to crates.io.

mod constants;
mod diagnostic;
mod expand;
mod literal;

use proc_macro::TokenStream;

use expand::{expand, Target};

#[cfg(feature = "spicedb")]
#[proc_macro]
pub fn object_id_impl(input: TokenStream) -> TokenStream {
    expand(input, Target::ObjectId)
}

#[cfg(feature = "spicedb")]
#[proc_macro]
pub fn object_type_impl(input: TokenStream) -> TokenStream {
    expand(input, Target::ObjectType)
}

#[proc_macro]
pub fn principal_kind_impl(input: TokenStream) -> TokenStream {
    expand(input, Target::PrincipalKind)
}

#[cfg(feature = "spicedb")]
#[proc_macro]
pub fn relation_name_impl(input: TokenStream) -> TokenStream {
    expand(input, Target::RelationName)
}

#[cfg(feature = "spicedb")]
#[proc_macro]
pub fn param_name_impl(input: TokenStream) -> TokenStream {
    expand(input, Target::ParamName)
}
