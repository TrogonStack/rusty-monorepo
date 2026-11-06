use trogon_claims::{PrincipalKind, SubjectId};

fn main() {
    assert_eq!(
        SubjectId::try_new("user@example.com#fragment").unwrap().as_str(),
        "user@example.com#fragment"
    );
    assert_eq!(PrincipalKind::try_new("person").unwrap().as_str(), "person");
    assert!(SubjectId::try_new("").is_err());
    assert!(PrincipalKind::try_new("Person").is_err());
}
