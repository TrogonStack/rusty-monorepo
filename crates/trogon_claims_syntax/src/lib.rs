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
//! | [`validate_principal_kind`] | An opaque, non-empty application classification of a principal |
//! | [`validate_subject_id`], [`validate_client_id`], [`validate_non_empty`] | Generic subject identifier, OAuth client id and non-empty text |
//!
//! ```
//! use trogon_claims_syntax::validate_subject_id;
//!
//! assert!(validate_subject_id("user@example.com").is_ok());
//! assert!(validate_subject_id("").is_err());
//! ```
//!
//! # Optional SpiceDB syntax
//!
//! The default-off `spicedb` feature adds `validate_object_id`,
//! `validate_object_type`, `validate_relation_name` and `validate_param_name`,
//! their limits and diagnostic rules. `trogon_claims/spicedb` forwards it.
//! Generic subject ids and principal kinds keep their independent contracts.
//!
//! Object ids follow [Authzed `ObjectReference.object_id`](https://github.com/authzed/api/blob/3270b08244f3e97e2a57a7ee2d9c649c88b861d3/authzed/api/v1/core.proto#L106-L115):
//! nonempty `[a-zA-Z0-9/_|\-=+]` text of at most 1024 bytes, or a standalone
//! wildcard. Concrete object ids deliberately exclude the wildcard as local policy.
//!
//! ```
//! # #[cfg(feature = "spicedb")]
//! # {
//! use trogon_claims_syntax::validate_object_id;
//! assert!(validate_object_id("user-1").is_ok());
//! assert!(validate_object_id("*").is_err());
//! # }
//! ```
//!
//! The crate is not published to crates.io. Depend on `trogon_claims` instead
//! unless you are writing another macro over the same rules.

#![no_std]

mod constants;
#[cfg(feature = "spicedb")]
mod object_id;
#[cfg(feature = "spicedb")]
mod object_type;
#[cfg(feature = "spicedb")]
mod param_name;
mod principal_kind;
#[cfg(feature = "spicedb")]
mod relation_name;
#[cfg(feature = "spicedb")]
mod segment;
mod text;
mod violation;

pub use constants::{CLIENT_ID_MAX_BYTE, CLIENT_ID_MIN_BYTE, OIDC_SUBJECT_MAX_BYTES};
#[cfg(feature = "spicedb")]
pub use constants::{
    OBJECT_ID_ALPHABET, OBJECT_ID_MAX_BYTES, OBJECT_TYPE_MAX_BYTES, PARAM_NAME_MAX_BYTES, RELATION_NAME_MAX_BYTES,
    SEGMENT_MIN_BYTES, TYPE_FINAL_SEGMENT_MAX_BYTES, TYPE_INNER_SEGMENT_MAX_BYTES, WILDCARD_BYTE,
};
#[cfg(feature = "spicedb")]
pub use object_id::validate_object_id;
#[cfg(feature = "spicedb")]
pub use object_type::validate_object_type;
#[cfg(feature = "spicedb")]
pub use param_name::validate_param_name;
pub use principal_kind::validate_principal_kind;
#[cfg(feature = "spicedb")]
pub use relation_name::validate_relation_name;
pub use text::{validate_client_id, validate_non_empty, validate_subject_id};
pub use violation::{SyntaxRule, SyntaxViolation};
