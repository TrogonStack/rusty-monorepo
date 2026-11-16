use trogon_claims_syntax::validate_subject_id;

use crate::errors::ValueKind;
use crate::value_object::string_value_object;

string_value_object!(SubjectId, ValueKind::SubjectId, validate_subject_id);
