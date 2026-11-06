use std::borrow::Cow;

use trogon_claims_syntax::validate_principal_kind;

use crate::errors::ValueKind;
use crate::value_object::string_value_object;

string_value_object!(
    /// An opaque application classification. Non-empty text is this crate's
    /// application contract, independent of token and backend identifier grammars.
    PrincipalKind,
    ValueKind::PrincipalKind,
    validate_principal_kind
);

impl PrincipalKind {
    #[doc(hidden)]
    pub const fn from_static_unchecked(value: &'static str) -> Self {
        assert!(validate_principal_kind(value).is_ok(), "invalid principal kind literal");
        Self(Cow::Borrowed(value))
    }
}
