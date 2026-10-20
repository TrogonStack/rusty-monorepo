use std::borrow::Cow;

use trogon_claims_syntax::validate_principal_kind;

use crate::errors::ValueKind;
use crate::value_object::string_value_object;

string_value_object!(PrincipalKind, ValueKind::PrincipalKind, validate_principal_kind);

impl PrincipalKind {
    #[doc(hidden)]
    pub const fn from_static_unchecked(value: &'static str) -> Self {
        Self(Cow::Borrowed(value))
    }
}
