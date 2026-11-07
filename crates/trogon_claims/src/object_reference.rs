use std::fmt;
use std::str::FromStr;

use crate::constants::OBJECT_SEPARATOR;
use crate::object_id::ObjectId;
use crate::object_type::ObjectType;
use crate::reference_parse_error::ReferenceParseError;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ObjectReference {
    object_type: ObjectType,
    object_id: ObjectId,
}

impl ObjectReference {
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

impl fmt::Display for ObjectReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{OBJECT_SEPARATOR}{}", self.object_type, self.object_id)
    }
}

impl FromStr for ObjectReference {
    type Err = ReferenceParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (object_type, object_id) = text
            .split_once(OBJECT_SEPARATOR)
            .ok_or(ReferenceParseError::MissingSeparator)?;
        Ok(Self::new(
            ObjectType::try_new(object_type.to_owned())?,
            ObjectId::try_new(object_id.to_owned())?,
        ))
    }
}
