use trogon_claims::{object_id, object_type, principal_kind, ObjectId, ObjectType, PrincipalKind};

fn main() {
    let id: ObjectId = object_id!("readme");
    let raw: ObjectId = object_id!(r"a/b_c|d-e=f+g");
    let kind: ObjectType = object_type!("myapp/document");
    let person: PrincipalKind = principal_kind!("person");
    assert_eq!(person.as_str(), "person");
    assert_eq!(id.as_str(), "readme");
    assert_eq!(raw.as_str(), "a/b_c|d-e=f+g");
    assert_eq!(kind.as_str(), "myapp/document");
}
