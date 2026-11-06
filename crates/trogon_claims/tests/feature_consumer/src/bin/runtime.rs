use trogon_claims::{ObjectId, ObjectType, PrincipalKind};

fn main() {
    assert_eq!(ObjectId::try_new("document-1").unwrap().as_str(), "document-1");
    assert_eq!(ObjectType::try_new("document").unwrap().as_str(), "document");
    assert_eq!(PrincipalKind::try_new("person").unwrap().as_str(), "person");
    assert!(ObjectId::try_new("*").is_err());
    assert!(ObjectType::try_new("Document").is_err());
    assert!(PrincipalKind::try_new("Person").is_err());
}
