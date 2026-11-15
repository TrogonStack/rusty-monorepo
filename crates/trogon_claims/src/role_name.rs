use trogon_claims_syntax::validate_non_empty;

use crate::errors::ValueKind;
use crate::value_object::string_value_object;

string_value_object!(RoleName, ValueKind::RoleName, validate_non_empty);
