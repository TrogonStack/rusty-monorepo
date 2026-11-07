use std::fmt;
use std::str::FromStr;

use crate::claim_set::SubjectRef;
use crate::constants::{OBJECT_SEPARATOR, RELATION_SEPARATOR};
use crate::object_id::ObjectId;
use crate::object_type::ObjectType;
use crate::reference_parse_error::ReferenceParseError;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SubjectReference {
    object_type: ObjectType,
    object_id: ObjectId,
}

impl SubjectReference {
    pub fn new(object_type: ObjectType, object_id: ObjectId) -> Self {
        Self { object_type, object_id }
    }

    pub fn object_type(&self) -> &ObjectType {
        &self.object_type
    }

    pub fn object_id(&self) -> &ObjectId {
        &self.object_id
    }
}

impl From<SubjectRef<'_>> for SubjectReference {
    fn from(subject: SubjectRef<'_>) -> Self {
        Self::new(subject.object_type().clone(), subject.id().clone())
    }
}

impl fmt::Display for SubjectReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{OBJECT_SEPARATOR}{}", self.object_type, self.object_id)
    }
}

impl FromStr for SubjectReference {
    type Err = ReferenceParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.contains(RELATION_SEPARATOR) {
            return Err(ReferenceParseError::UnexpectedRelation);
        }
        let (object_type, object_id) = text
            .split_once(OBJECT_SEPARATOR)
            .ok_or(ReferenceParseError::MissingSeparator)?;
        Ok(Self::new(
            ObjectType::try_new(object_type.to_owned())?,
            ObjectId::try_new(object_id.to_owned())?,
        ))
    }
}
