use std::borrow::Cow;

use trogon_claims_syntax::validate_non_empty;

use crate::constants::LOCAL_AUTHORITY;
use crate::errors::ValueKind;
use crate::value_object::string_value_object;

string_value_object!(Issuer, ValueKind::Issuer, validate_non_empty);

impl Issuer {
    pub(crate) const fn local_authority() -> Self {
        Self(Cow::Borrowed(LOCAL_AUTHORITY))
    }
}
