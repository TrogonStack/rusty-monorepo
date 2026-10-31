use crate::claim_set::SubjectRef;
use crate::sealed::Sealed;
use crate::subject_reference::SubjectReference;

pub trait AuthorizationSubject: Sealed {
    fn subject_reference(&self) -> SubjectReference;
}

impl Sealed for SubjectReference {}
impl Sealed for SubjectRef<'_> {}

impl AuthorizationSubject for SubjectReference {
    fn subject_reference(&self) -> SubjectReference {
        self.clone()
    }
}

impl AuthorizationSubject for SubjectRef<'_> {
    fn subject_reference(&self) -> SubjectReference {
        SubjectReference::from(self.clone())
    }
}
