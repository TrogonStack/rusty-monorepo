use std::fmt;
use std::str::FromStr;

use crate::constants::RELATION_SEPARATOR;
use crate::reference_parse_error::ReferenceParseError;
use crate::relation_name::RelationName;
use crate::subject_reference::SubjectReference;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SubjectSet {
    subject: SubjectReference,
    relation: RelationName,
}

impl SubjectSet {
    pub fn new(subject: SubjectReference, relation: RelationName) -> Self {
        Self { subject, relation }
    }

    pub fn subject(&self) -> &SubjectReference {
        &self.subject
    }

    pub fn relation(&self) -> &RelationName {
        &self.relation
    }
}

impl fmt::Display for SubjectSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{RELATION_SEPARATOR}{}", self.subject, self.relation)
    }
}

impl FromStr for SubjectSet {
    type Err = ReferenceParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (subject, relation) = text
            .split_once(RELATION_SEPARATOR)
            .ok_or(ReferenceParseError::MissingRelation)?;
        Ok(Self::new(
            subject.parse::<SubjectReference>()?,
            RelationName::try_new(relation.to_owned())?,
        ))
    }
}
