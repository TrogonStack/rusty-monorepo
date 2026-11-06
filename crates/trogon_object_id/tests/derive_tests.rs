#![cfg(feature = "derive")]

use trogon_object_id::{ObjectId, ObjectIdDomain, ObjectIdUnion};

#[derive(ObjectIdDomain)]
#[object_id(prefix = "user")]
struct UserDomain;
type UserId = ObjectId<UserDomain>;

#[derive(ObjectIdDomain)]
#[object_id(prefix = "service")]
struct ServiceDomain;
type ServiceId = ObjectId<ServiceDomain>;

#[derive(Debug, Clone, PartialEq, Eq, ObjectIdUnion)]
enum PrincipalId {
    User(UserId),
    Service(ServiceId),
}

#[test]
fn derived_domain_uses_its_declared_prefix() {
    let user = UserId::try_from_raw("abc-123").expect("valid suffix");
    assert_eq!(user.to_string(), "user_abc-123", "the declaration defines the domain prefix");
}

#[test]
fn derived_union_parses_and_formats_normal_enum_variants() {
    let principal = "user_abc-123".parse::<PrincipalId>().expect("known domain prefix");
    assert!(matches!(principal, PrincipalId::User(_)), "prefix selects the typed variant");
    assert_eq!(principal.to_string(), "user_abc-123", "formatted union retains its discriminator");
}
