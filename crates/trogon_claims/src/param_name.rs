use std::borrow::Cow;

use trogon_claims_syntax::validate_param_name;

use crate::errors::ValueKind;
use crate::value_object::string_value_object;

string_value_object!(ParamName, ValueKind::ParamName, validate_param_name);

impl ParamName {
    #[doc(hidden)]
    pub const fn from_static_unchecked(value: &'static str) -> Self {
        Self(Cow::Borrowed(value))
    }
}
