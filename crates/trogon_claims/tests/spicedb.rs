#![cfg(feature = "spicedb")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::Duration;

use prost::Message;
use prost_types::value::Kind;
use prost_types::Struct;
use trogon_claims::{
    object_id, object_type, param_name, relation_name, ActorIssuer, ActorKey, Base64UrlUnpadded, CaveatContext,
    CaveatContextError, CheckContext, CheckOutcome, CheckRequest, ClaimPropertyKey, ClaimValue, CurrentActor,
    LookupOutcome, ObjectId, ObjectReference, OutcomeError, ParamDeclaration, ParamName, ReferenceParseError,
    RelationName, SubjectId, SubjectReference, SubjectSet, Timestamp, Unsigned63, UtcOffset, ValueKind, WriteContext,
};

fn declaration(names: &[&'static str]) -> ParamDeclaration {
    ParamDeclaration::new(names.iter().map(|name| ParamName::try_new(*name).unwrap()))
}

fn project(context: &CheckContext) -> Result<Struct, CaveatContextError> {
    Struct::try_from(context)
}

fn single(name: &'static str, value: ClaimValue) -> CheckContext {
    let declared = declaration(&[name]);
    let mut builder = CaveatContext::builder(&declared);
    builder.insert(ParamName::try_new(name).unwrap(), value).unwrap();
    builder.build()
}

fn kind_of(projected: &Struct, name: &str) -> Kind {
    projected.fields[name].kind.clone().unwrap()
}

#[test]
fn subject_reference_round_trips_its_text_form() {
    let subject: SubjectReference = "user:01HQZX9K3M".parse().unwrap();
    assert_eq!(subject.object_type().as_str(), "user");
    assert_eq!(subject.object_id().as_str(), "01HQZX9K3M");
    assert_eq!(subject.to_string(), "user:01HQZX9K3M");
    let nested: SubjectReference = "team/member:abc_123".parse().unwrap();
    assert_eq!(nested.to_string().parse::<SubjectReference>().unwrap(), nested);
}

#[test]
fn a_wildcard_subject_cannot_be_produced() {
    let error = "user:*".parse::<SubjectReference>().unwrap_err();
    assert!(matches!(error, ReferenceParseError::Invalid(ref inner) if inner.kind() == ValueKind::ObjectId));
    assert!(ObjectId::try_new("*").is_err());
}

#[test]
fn a_subject_reference_refuses_a_relation() {
    assert_eq!(
        "group:eng#member".parse::<SubjectReference>().unwrap_err(),
        ReferenceParseError::UnexpectedRelation
    );
    assert_eq!(
        "usernoseparator".parse::<SubjectReference>().unwrap_err(),
        ReferenceParseError::MissingSeparator
    );
}

#[test]
fn a_subject_set_round_trips_and_needs_its_relation() {
    let set: SubjectSet = "group:eng#member".parse().unwrap();
    assert_eq!(set.subject().to_string(), "group:eng");
    assert_eq!(set.relation().as_str(), "member");
    assert_eq!(set.to_string(), "group:eng#member");
    assert_eq!(
        "group:eng".parse::<SubjectSet>().unwrap_err(),
        ReferenceParseError::MissingRelation
    );
    assert!("group:*#member".parse::<SubjectSet>().is_err());
    assert!("group:eng#Member".parse::<SubjectSet>().is_err());
}

#[test]
fn object_reference_parses_and_displays() {
    let resource: ObjectReference = "document/page:abc-1".parse().unwrap();
    assert_eq!(resource.to_string(), "document/page:abc-1");
}

#[test]
fn relation_and_param_names_validate() {
    assert!(RelationName::try_new("view").is_ok());
    assert!(RelationName::try_new("v").is_err());
    assert!(RelationName::try_new("View").is_err());
    assert_eq!(relation_name!("can_edit").as_str(), "can_edit");
    assert!(ParamName::try_new("user_ip").is_ok());
    assert!(ParamName::try_new("user-ip").is_err());
    assert_eq!(param_name!("user_ip").as_str(), "user_ip");
}

#[test]
fn the_check_outcome_has_three_arms_and_unspecified_is_a_protocol_error() {
    assert_eq!(CheckOutcome::try_from_wire(1, []).unwrap(), CheckOutcome::NoPermission);
    assert_eq!(CheckOutcome::try_from_wire(2, []).unwrap(), CheckOutcome::HasPermission);
    assert_eq!(
        CheckOutcome::try_from_wire(3, ["user_ip", "tenant"]).unwrap(),
        CheckOutcome::Conditional {
            missing: vec![param_name!("user_ip"), param_name!("tenant")]
        }
    );
    assert_eq!(
        CheckOutcome::try_from_wire(0, []).unwrap_err(),
        OutcomeError::Unspecified
    );
    assert_eq!(
        CheckOutcome::try_from_wire(9, []).unwrap_err(),
        OutcomeError::Unknown { value: 9 }
    );
    assert!(matches!(
        CheckOutcome::try_from_wire(3, ["not valid"]).unwrap_err(),
        OutcomeError::MissingParameter(_)
    ));
}

#[test]
fn the_lookup_outcome_has_no_denial_arm() {
    assert_eq!(
        LookupOutcome::try_from_wire(1, []).unwrap(),
        LookupOutcome::HasPermission
    );
    assert_eq!(
        LookupOutcome::try_from_wire(2, ["tenant"]).unwrap(),
        LookupOutcome::Conditional {
            missing: vec![param_name!("tenant")]
        }
    );
    assert_eq!(
        LookupOutcome::try_from_wire(0, []).unwrap_err(),
        OutcomeError::Unspecified
    );
    assert_eq!(
        LookupOutcome::try_from_wire(3, []).unwrap_err(),
        OutcomeError::Unknown { value: 3 }
    );
}

#[test]
fn undeclared_and_duplicate_parameters_are_refused() {
    let declared = declaration(&["tenant"]);
    let mut builder = CheckContext::builder(&declared);
    assert_eq!(
        builder.insert(param_name!("tennant"), true).unwrap_err(),
        CaveatContextError::Undeclared {
            parameter: param_name!("tennant")
        }
    );
    builder.insert(param_name!("tenant"), true).unwrap();
    assert_eq!(
        builder.insert(param_name!("tenant"), false).unwrap_err(),
        CaveatContextError::Duplicate {
            parameter: param_name!("tenant")
        }
    );
}

#[test]
fn a_top_level_integer_is_a_decimal_string() {
    let projected = project(&single("amount", ClaimValue::Integer(i64::MAX))).unwrap();
    assert_eq!(
        kind_of(&projected, "amount"),
        Kind::StringValue("9223372036854775807".to_owned())
    );
    let projected = project(&single("amount", ClaimValue::Integer(-7))).unwrap();
    assert_eq!(kind_of(&projected, "amount"), Kind::StringValue("-7".to_owned()));
    let projected = project(&single("amount", ClaimValue::Unsigned(Unsigned63::try_new(5).unwrap()))).unwrap();
    assert_eq!(kind_of(&projected, "amount"), Kind::StringValue("5".to_owned()));
}

#[test]
fn a_nested_integer_above_two_to_the_fifty_third_is_unrepresentable() {
    let exact = 1_i64 << 53;
    let ok = ClaimValue::List(vec![ClaimValue::Integer(exact), ClaimValue::Integer(-exact)]);
    assert!(project(&single("ids", ok)).is_ok());

    let over = ClaimValue::List(vec![ClaimValue::Integer(exact + 1)]);
    assert_eq!(
        project(&single("ids", over)).unwrap_err(),
        CaveatContextError::NestedIntegerPrecision {
            parameter: param_name!("ids")
        }
    );

    let key = ClaimPropertyKey::try_new("account").unwrap();
    let map = ClaimValue::Map(BTreeMap::from([(key, ClaimValue::Integer(i64::MIN))]));
    assert!(matches!(
        project(&single("owner", map)).unwrap_err(),
        CaveatContextError::NestedIntegerPrecision { .. }
    ));

    let unsigned = ClaimValue::List(vec![ClaimValue::Unsigned(Unsigned63::try_new(1 << 60).unwrap())]);
    assert!(project(&single("ids", unsigned)).is_err());
}

#[test]
fn timestamps_always_carry_an_offset() {
    let offset = UtcOffset::try_from_minutes(-300).unwrap();
    let timestamp = Timestamp::try_new(1_700_000_000, 500_000_000, offset).unwrap();
    let projected = project(&single("at", ClaimValue::Timestamp(timestamp))).unwrap();
    let Kind::StringValue(text) = kind_of(&projected, "at") else {
        panic!("not a string");
    };
    assert!(text.ends_with("-05:00"), "{text}");
    let utc = Timestamp::try_new(0, 0, UtcOffset::UTC).unwrap();
    let projected = project(&single("at", ClaimValue::Timestamp(utc))).unwrap();
    assert_eq!(
        kind_of(&projected, "at"),
        Kind::StringValue("1970-01-01T00:00:00Z".to_owned())
    );
}

#[test]
fn bytes_become_padded_standard_base64() {
    for (bytes, expected) in [
        (vec![], ""),
        (vec![0xfb], "+w=="),
        (vec![0xfb, 0xff], "+/8="),
        (vec![0xfb, 0xff, 0xfe], "+//+"),
        (b"foobar".to_vec(), "Zm9vYmFy"),
    ] {
        let projected = project(&single("blob", ClaimValue::Bytes(bytes))).unwrap();
        assert_eq!(kind_of(&projected, "blob"), Kind::StringValue(expected.to_owned()));
    }
}

#[test]
fn the_remaining_arms_project_to_their_natural_kinds() {
    let projected = project(&single("ip", ClaimValue::IpAddr("10.0.0.1".parse::<IpAddr>().unwrap()))).unwrap();
    assert_eq!(kind_of(&projected, "ip"), Kind::StringValue("10.0.0.1".to_owned()));
    let projected = project(&single("ok", ClaimValue::Boolean(true))).unwrap();
    assert_eq!(kind_of(&projected, "ok"), Kind::BoolValue(true));
    let projected = project(&single("ratio", ClaimValue::Double(0.5))).unwrap();
    assert_eq!(kind_of(&projected, "ratio"), Kind::NumberValue(0.5));
    let projected = project(&single("ttl", ClaimValue::Duration(Duration::new(90, 500_000_000)))).unwrap();
    assert_eq!(kind_of(&projected, "ttl"), Kind::StringValue("90.5s".to_owned()));
    let projected = project(&single("ttl", ClaimValue::Duration(Duration::from_secs(60)))).unwrap();
    assert_eq!(kind_of(&projected, "ttl"), Kind::StringValue("60s".to_owned()));
}

#[test]
fn a_non_finite_double_is_refused() {
    assert_eq!(
        project(&single("ratio", ClaimValue::Double(f64::NAN))).unwrap_err(),
        CaveatContextError::NonFiniteDouble {
            parameter: param_name!("ratio")
        }
    );
}

#[test]
fn the_budget_is_measured_in_encoded_protobuf_size() {
    let text = ClaimValue::Text("x".repeat(100).into());
    let declared = declaration(&["note"]);
    let mut builder = CheckContext::builder(&declared);
    builder.insert(param_name!("note"), text.clone()).unwrap();
    let context = builder.build();
    let projected = project(&context).unwrap();
    let encoded = projected.encoded_len();
    assert!(encoded > 100);

    let mut tight = CheckContext::builder(&declared).with_budget(encoded - 1);
    tight.insert(param_name!("note"), text.clone()).unwrap();
    assert_eq!(
        project(&tight.build()).unwrap_err(),
        CaveatContextError::OverBudget {
            encoded,
            budget: encoded - 1
        }
    );

    let mut exact = CheckContext::builder(&declared).with_budget(encoded);
    exact.insert(param_name!("note"), text).unwrap();
    assert!(project(&exact.build()).is_ok());
}

#[test]
fn the_default_budget_is_four_kibibytes() {
    let declared = declaration(&["note"]);
    assert_eq!(CheckContext::builder(&declared).build().budget(), 4096);
    let mut builder = CheckContext::builder(&declared);
    builder
        .insert(param_name!("note"), ClaimValue::Text("x".repeat(5000).into()))
        .unwrap();
    assert!(matches!(
        project(&builder.build()).unwrap_err(),
        CaveatContextError::OverBudget { budget: 4096, .. }
    ));
}

#[test]
fn write_time_context_projects_through_the_same_conversion() {
    let declared = declaration(&["tenant"]);
    let mut builder = WriteContext::builder(&declared);
    builder
        .insert(param_name!("tenant"), ClaimValue::Text("acme".into()))
        .unwrap();
    let projected = Struct::try_from(&builder.build()).unwrap();
    assert_eq!(kind_of(&projected, "tenant"), Kind::StringValue("acme".to_owned()));
}

#[test]
fn a_check_request_takes_the_subject_reference_and_the_current_actor_path() {
    let resource: ObjectReference = "document:readme".parse().unwrap();
    let subject: SubjectReference = "user:alice".parse().unwrap();
    let request = CheckRequest::new(resource, relation_name!("view"), &subject, None);
    assert_eq!(request.subject(), &subject);
    assert_eq!(request.permission().as_str(), "view");
    assert!(request.context().is_none());

    let actor = CurrentActor::try_new(
        ActorKey::Subject {
            issuer: ActorIssuer::Unstated,
            sub: SubjectId::try_new("bob").unwrap(),
        },
        Vec::new(),
        None,
    )
    .unwrap();
    let derived =
        SubjectReference::try_from_current_actor::<Base64UrlUnpadded>(&actor, object_type!("service")).unwrap();
    assert_eq!(derived.object_type().as_str(), "service");
    assert_eq!(derived.object_id(), &object_id!("Ym9i"));

    let client = CurrentActor::try_new(
        ActorKey::Client {
            issuer: ActorIssuer::Unstated,
            client_id: trogon_claims::ClientId::try_new("cli").unwrap(),
        },
        Vec::new(),
        None,
    )
    .unwrap();
    assert!(SubjectReference::try_from_current_actor::<Base64UrlUnpadded>(&client, object_type!("service")).is_err());
}
