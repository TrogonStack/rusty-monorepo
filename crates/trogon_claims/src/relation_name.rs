use std::borrow::Cow;

use trogon_claims_syntax::validate_relation_name;

use crate::errors::ValueKind;
use crate::value_object::string_value_object;

string_value_object!(RelationName, ValueKind::RelationName, validate_relation_name);

impl RelationName {
    #[doc(hidden)]
    pub const fn from_static_unchecked(value: &'static str) -> Self {
        Self(Cow::Borrowed(value))
    }
}
