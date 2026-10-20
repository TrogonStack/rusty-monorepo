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
