//! Dependency-free validators for the identifier shapes shared by `trogon_claims`
//! and its literal macros.
//!
//! Each `validate_*` function checks one text against one rule set and returns a
//! [`SyntaxViolation`] naming the rule that failed, or `Ok(())`. The functions are
//! `const fn`, allocate nothing, and the crate is `no_std` with no
//! dependencies, so the same rules run at compile time inside a procedural macro
//! and at run time inside a constructor.
//!
//! | Validator | Accepts |
//! |---|---|
//! | [`validate_object_id`] | A SpiceDB object id, up to [`OBJECT_ID_MAX_BYTES`] bytes, never the wildcard |
//! | [`validate_object_type`] | A SpiceDB object type, optionally namespaced by `/` segments |
//! | [`validate_relation_name`] | One lowercase segment of 3 to [`RELATION_NAME_MAX_BYTES`] bytes |
//! | [`validate_param_name`] | A CEL identifier of up to [`PARAM_NAME_MAX_BYTES`] bytes |
//! | [`validate_principal_kind`] | An application classification of a principal |
//! | [`validate_subject_id`], [`validate_client_id`], [`validate_non_empty`] | OIDC subject, client id and non-empty text |
//!
//! ```
//! use trogon_claims_syntax::validate_object_id;
//!
//! assert!(validate_object_id("user-1").is_ok());
//! assert!(validate_object_id("").is_err());
//! assert!(validate_object_id("*").is_err());
//! ```
//!
//! The crate is not published to crates.io. Depend on `trogon_claims` instead
//! unless you are writing another macro over the same rules.

#![no_std]

mod constants;
mod object_id;
mod object_type;
mod param_name;
mod principal_kind;
mod relation_name;
mod segment;
mod text;
mod violation;

pub use constants::{
    CLIENT_ID_MAX_BYTE, CLIENT_ID_MIN_BYTE, OBJECT_ID_ALPHABET, OBJECT_ID_MAX_BYTES, OBJECT_TYPE_MAX_BYTES,
    OIDC_SUBJECT_MAX_BYTES, PARAM_NAME_MAX_BYTES, PRINCIPAL_KIND_MAX_BYTES, RELATION_NAME_MAX_BYTES, SEGMENT_MIN_BYTES,
    TYPE_FINAL_SEGMENT_MAX_BYTES, TYPE_INNER_SEGMENT_MAX_BYTES, WILDCARD_BYTE,
};
pub use object_id::validate_object_id;
pub use object_type::validate_object_type;
pub use param_name::validate_param_name;
pub use principal_kind::validate_principal_kind;
pub use relation_name::validate_relation_name;
pub use text::{validate_client_id, validate_non_empty, validate_subject_id};
pub use violation::{SyntaxRule, SyntaxViolation};
