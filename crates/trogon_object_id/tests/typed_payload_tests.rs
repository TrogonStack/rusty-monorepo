use trogon_object_id::{validators, Definition, Domain, ObjectId};

struct TextDomain;
impl Domain for TextDomain {
    const DEFINITION: Definition = Definition::new("text", "_");
    type Validator = validators::Any;
}

#[test]
fn owned_text_construction_reuses_the_callers_buffer() {
    let raw = String::from("owned-text-value");
    let original = raw.as_ptr();
    let id = ObjectId::<TextDomain>::try_from_owned(raw).expect("valid text");
    assert_eq!(id.as_raw().as_ptr(), original, "owned storage should not be copied");
}

#[cfg(feature = "uuid")]
struct SessionDomain;
#[cfg(feature = "uuid")]
impl Domain for SessionDomain {
    const DEFINITION: Definition = Definition::new("session", "_");
    type Validator = validators::Uuid;
}

#[cfg(feature = "uuid")]
#[test]
fn uuid_domain_uses_only_its_binary_payload() {
    assert_eq!(std::mem::size_of::<ObjectId<SessionDomain>>(), std::mem::size_of::<uuid::Uuid>(), "UUID identifiers should not carry a heap string");
}
