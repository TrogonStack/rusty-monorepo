use trogon_claims::{
    Base64UrlUnpadded, ClaimSet, HasSubject, ObjectId, ObjectType, SubjectId, SubjectMapping, SubjectMappingError,
    SubjectRef, Verbatim,
};

struct UserClaims(ObjectId);

impl ClaimSet for UserClaims {}

impl HasSubject for UserClaims {
    const SUBJECT_TYPE: ObjectType = ObjectType::from_static_unchecked("user");
    type Mapping = Verbatim;

    fn subject(&self) -> SubjectRef<'_> {
        SubjectRef::new(Self::SUBJECT_TYPE, &self.0)
    }
}

fn map<M: SubjectMapping>(subject: &SubjectId) -> Result<ObjectId, SubjectMappingError> {
    M::map(subject)
}

fn main() {
    let subject = SubjectId::try_new("user-1").unwrap();
    let claims = UserClaims(map::<Verbatim>(&subject).unwrap());
    assert_eq!(claims.subject().object_type().as_str(), "user");
    assert_eq!(claims.subject().id().as_str(), "user-1");
    assert!(map::<Base64UrlUnpadded>(&SubjectId::try_new("user@example.com").unwrap()).is_ok());
}
