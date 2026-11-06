use trogon_claims::PrincipalKind;

#[cfg(feature = "macros")]
use trogon_claims::principal_kind;

#[cfg(feature = "spicedb")]
use trogon_claims::{ObjectId, ObjectType, ParamName, RelationName};

#[cfg(all(feature = "spicedb", feature = "macros"))]
use trogon_claims::{object_id, object_type, param_name, relation_name};

#[cfg(feature = "spicedb")]
#[test]
#[should_panic(expected = "invalid object id literal")]
fn static_object_id_rejects_the_wildcard() {
    let _ = ObjectId::from_static_unchecked("*");
}

#[cfg(feature = "spicedb")]
#[test]
#[should_panic(expected = "invalid object type literal")]
fn static_object_type_rejects_invalid_grammar() {
    let _ = ObjectType::from_static_unchecked("Document");
}

#[test]
#[should_panic(expected = "invalid principal kind literal")]
fn static_principal_kind_rejects_empty_text() {
    let _ = PrincipalKind::from_static_unchecked("");
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
fn valid_static_construction_remains_const_capable() {
    const KIND: PrincipalKind = PrincipalKind::from_static_unchecked("person");
    assert_eq!(KIND.as_str(), "person");
    const OPAQUE_KIND: PrincipalKind = PrincipalKind::from_static_unchecked("人/Person");
    assert_eq!(OPAQUE_KIND.as_str(), "人/Person");

    #[cfg(feature = "spicedb")]
    {
        const ID: ObjectId = ObjectId::from_static_unchecked("readme");
        const TYPE: ObjectType = ObjectType::from_static_unchecked("document");
        assert_eq!(ID.as_str(), "readme");
        assert_eq!(TYPE.as_str(), "document");
        const RELATION: RelationName = RelationName::from_static_unchecked("can_read");
        const PARAMETER: ParamName = ParamName::from_static_unchecked("tenant_id");
        assert_eq!(RELATION.as_str(), "can_read");
        assert_eq!(PARAMETER.as_str(), "tenant_id");
    }
}

#[cfg(feature = "macros")]
#[test]
fn literal_macros_remain_const_capable() {
    const KIND: PrincipalKind = principal_kind!("person");
    assert_eq!(KIND, PrincipalKind::from_static_unchecked("person"));

    #[cfg(feature = "spicedb")]
    {
        const ID: ObjectId = object_id!("readme");
        const TYPE: ObjectType = object_type!("document");
        assert_eq!(ID, ObjectId::from_static_unchecked("readme"));
        assert_eq!(TYPE, ObjectType::from_static_unchecked("document"));
        const RELATION: RelationName = relation_name!("can_read");
        const PARAMETER: ParamName = param_name!("tenant_id");
        assert_eq!(RELATION, RelationName::from_static_unchecked("can_read"));
        assert_eq!(PARAMETER, ParamName::from_static_unchecked("tenant_id"));
    }
}
