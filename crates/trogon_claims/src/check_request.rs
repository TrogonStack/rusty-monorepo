use crate::authorization_subject::AuthorizationSubject;
use crate::caveat_context::CheckContext;
use crate::object_reference::ObjectReference;
use crate::relation_name::RelationName;
use crate::subject_reference::SubjectReference;

#[derive(Clone, Debug, PartialEq)]
pub struct CheckRequest {
    resource: ObjectReference,
    permission: RelationName,
    subject: SubjectReference,
    context: Option<CheckContext>,
}

impl CheckRequest {
    pub fn new(
        resource: ObjectReference,
        permission: RelationName,
        subject: &impl AuthorizationSubject,
        context: Option<CheckContext>,
    ) -> Self {
        Self {
            resource,
            permission,
            subject: subject.subject_reference(),
            context,
        }
    }

    pub fn resource(&self) -> &ObjectReference {
        &self.resource
    }

    pub fn permission(&self) -> &RelationName {
        &self.permission
    }

    pub fn subject(&self) -> &SubjectReference {
        &self.subject
    }

    pub fn context(&self) -> Option<&CheckContext> {
        self.context.as_ref()
    }
}
