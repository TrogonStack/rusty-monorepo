use trogon_claims_syntax::validate_client_id;

use crate::errors::ValueKind;
use crate::value_object::string_value_object;

string_value_object!(ClientId, ValueKind::ClientId, validate_client_id);
