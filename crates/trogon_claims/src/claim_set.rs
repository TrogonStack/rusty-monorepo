use crate::object_id::ObjectId;
use crate::object_type::ObjectType;
use crate::role_name::RoleName;
use crate::subject_mapping::SubjectMapping;

pub trait ClaimSet {}

pub trait HasSubject: ClaimSet {
    const SUBJECT_TYPE: ObjectType;
    type Mapping: SubjectMapping;

    fn subject(&self) -> SubjectRef<'_>;
}

pub trait HasRoles: ClaimSet {
    fn roles(&self) -> &[RoleName];
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubjectRef<'a> {
    object_type: ObjectType,
    id: &'a ObjectId,
}

impl<'a> SubjectRef<'a> {
    pub fn new(object_type: ObjectType, id: &'a ObjectId) -> Self {
        Self { object_type, id }
    }

    pub fn object_type(&self) -> &ObjectType {
        &self.object_type
    }

    pub fn id(&self) -> &ObjectId {
        self.id
    }
}
