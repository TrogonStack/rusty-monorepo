use crate::principal_kind::PrincipalKind;
use crate::subject_id::SubjectId;

/// A principal named by a claim nobody verified. It is for audit trails and
/// never for an authorization decision, so no authorization input accepts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnverifiedDirectedPrincipal {
    subject: SubjectId,
    kind: Option<PrincipalKind>,
}

impl UnverifiedDirectedPrincipal {
    pub fn new(subject: SubjectId, kind: Option<PrincipalKind>) -> Self {
        Self { subject, kind }
    }

    pub fn subject(&self) -> &SubjectId {
        &self.subject
    }

    pub fn kind(&self) -> Option<&PrincipalKind> {
        self.kind.as_ref()
    }
}
