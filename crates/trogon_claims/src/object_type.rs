use std::borrow::Cow;

use trogon_claims_syntax::validate_object_type;

use crate::errors::ValueKind;
use crate::value_object::string_value_object;

string_value_object!(ObjectType, ValueKind::ObjectType, validate_object_type);

impl ObjectType {
    #[doc(hidden)]
    pub const fn from_static_unchecked(value: &'static str) -> Self {
        Self(Cow::Borrowed(value))
    }
}
