use std::borrow::Cow;

use trogon_claims_syntax::validate_object_id;

use crate::errors::ValueKind;
use crate::value_object::string_value_object;

string_value_object!(ObjectId, ValueKind::ObjectId, validate_object_id);

impl ObjectId {
    #[doc(hidden)]
    pub const fn from_static_unchecked(value: &'static str) -> Self {
        Self(Cow::Borrowed(value))
    }
}

impl ObjectId {
    pub fn from_bytes_base64url_unpadded(bytes: &[u8]) -> Result<Self, crate::errors::ValueObjectError> {
        Self::try_new(crate::base64url::encode_unpadded(bytes))
    }
}
