use trogon_claims::{object_id, object_type, principal_kind, ObjectId, ObjectType, PrincipalKind};

fn main() {
    const ID: ObjectId = object_id!("document-1");
    const TYPE: ObjectType = object_type!("document");
    const KIND: PrincipalKind = principal_kind!("person");
    assert_eq!(ID.as_str(), "document-1");
    assert_eq!(TYPE.as_str(), "document");
    assert_eq!(KIND.as_str(), "person");
}
