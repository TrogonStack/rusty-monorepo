use trogon_claims::{object_id, object_type, principal_kind, ObjectId, ObjectType, PrincipalKind};

#[cfg(feature = "spicedb")]
use trogon_claims::{param_name, relation_name, ParamName, RelationName};

#[test]
#[should_panic(expected = "invalid object id literal")]
fn static_object_id_rejects_the_wildcard() {
    let _ = ObjectId::from_static_unchecked("*");
}

#[test]
#[should_panic(expected = "invalid object type literal")]
fn static_object_type_rejects_invalid_grammar() {
    let _ = ObjectType::from_static_unchecked("Document");
}

#[test]
#[should_panic(expected = "invalid principal kind literal")]
fn static_principal_kind_rejects_invalid_grammar() {
    let _ = PrincipalKind::from_static_unchecked("Person");
}

#[cfg(feature = "spicedb")]
#[test]
#[should_panic(expected = "invalid relation name literal")]
fn static_relation_name_rejects_invalid_grammar() {
    let _ = RelationName::from_static_unchecked("can:read");
}

#[cfg(feature = "spicedb")]
#[test]
#[should_panic(expected = "invalid parameter name literal")]
fn static_parameter_name_rejects_invalid_grammar() {
    let _ = ParamName::from_static_unchecked("tenant-id");
}

#[test]
fn valid_static_construction_and_literal_macros_remain_const_capable() {
    const ID: ObjectId = ObjectId::from_static_unchecked("readme");
    const TYPE: ObjectType = ObjectType::from_static_unchecked("document");
    const KIND: PrincipalKind = PrincipalKind::from_static_unchecked("person");
    const MACRO_ID: ObjectId = object_id!("readme");
    const MACRO_TYPE: ObjectType = object_type!("document");
    const MACRO_KIND: PrincipalKind = principal_kind!("person");
    assert_eq!(ID, MACRO_ID);
    assert_eq!(TYPE, MACRO_TYPE);
    assert_eq!(KIND, MACRO_KIND);

    #[cfg(feature = "spicedb")]
    {
        const RELATION: RelationName = RelationName::from_static_unchecked("can_read");
        const PARAMETER: ParamName = ParamName::from_static_unchecked("tenant_id");
        const MACRO_RELATION: RelationName = relation_name!("can_read");
        const MACRO_PARAMETER: ParamName = param_name!("tenant_id");
        assert_eq!(RELATION, MACRO_RELATION);
        assert_eq!(PARAMETER, MACRO_PARAMETER);
    }
}
