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
//! The crate is not published to crates.io. Depend on `trogon_claims` instead
//! unless you are writing another macro over the same rules.

#![no_std]

mod constants;
mod principal_kind;
mod text;
mod violation;

pub use constants::{CLIENT_ID_MAX_BYTE, CLIENT_ID_MIN_BYTE, OIDC_SUBJECT_MAX_BYTES};
pub use principal_kind::validate_principal_kind;
pub use text::{validate_client_id, validate_non_empty, validate_subject_id};
pub use violation::{SyntaxRule, SyntaxViolation};
