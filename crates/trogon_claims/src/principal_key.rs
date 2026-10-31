use crate::issuer::Issuer;
use crate::subject_id::SubjectId;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PrincipalKey {
    issuer: Issuer,
    sub: SubjectId,
}

impl PrincipalKey {
    pub fn new(issuer: Issuer, sub: SubjectId) -> Self {
        Self { issuer, sub }
    }

    pub fn issuer(&self) -> &Issuer {
        &self.issuer
    }

    pub fn sub(&self) -> &SubjectId {
        &self.sub
    }
}
