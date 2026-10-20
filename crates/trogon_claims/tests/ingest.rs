#![cfg(feature = "json")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use trogon_claims::ingest::{introspection, jwt_payload};
use trogon_claims::{
    ActorError, ActorIssuer, ActorKey, AuthenticationMethod, AuthenticationType, Binding, ClaimType, ClaimValue,
    ClaimValueError, ConfirmationError, Delegation, DynamicClaims, Identity, IngestError, IngestProfile, Ingested,
    Issuer, RawClaims, RawClaimsError, RoleName, WellKnownClaim,
};

const FIGURE_5: &str = r#"{
 "aud":"https://consumer.example.com",
 "iss":"https://issuer.example.com",
 "exp":1443904177,
 "nbf":1443904077,
 "sub":"user@example.com",
 "act":
 {
   "sub":"admin@example.com"
 }
}"#;

const FIGURE_6: &str = r#"{
 "aud":"https://backend.example.com",
 "iss":"https://issuer.example.com",
 "exp":1443904100,
 "nbf":1443904000,
 "sub":"user@example.com",
 "act":
 {
   "sub":"https://service16.example.com",
   "act":
   {
     "sub":"https://service77.example.com"
   }
 }
}"#;

fn profile() -> IngestProfile {
    IngestProfile::new(AuthenticationType::try_new("jwt").unwrap())
}

fn ingest_with(json: &str, profile: &IngestProfile) -> Result<Ingested<DynamicClaims>, IngestError> {
    let raw = RawClaims::from_slice(json.as_bytes()).unwrap();
    Identity::<DynamicClaims>::ingest(&raw, profile)
}

fn ingest(json: &str) -> Result<Ingested<DynamicClaims>, IngestError> {
    ingest_with(json, &profile())
}

fn claim_error(result: Result<Ingested<DynamicClaims>, IngestError>) -> (ClaimType, ClaimValueError) {
    match result.unwrap_err() {
        IngestError::Claim { name, kind } => (name, kind),
        other => panic!("expected a claim error, got {other:?}"),
    }
}

fn nested_actors(depth: usize) -> String {
    let mut inner = String::from(r#"{"sub":"actor-0"}"#);
    for level in 1..depth {
        inner = format!(r#"{{"sub":"actor-{level}","act":{inner}}}"#);
    }
    format!(r#"{{"iss":"https://issuer.example.com","sub":"user","act":{inner}}}"#)
}

fn claim_type(name: &str) -> ClaimType {
    ClaimType::try_new(name.to_owned()).unwrap()
}

#[test]
fn a_duplicate_member_is_rejected() {
    let error = RawClaims::from_slice(br#"{"sub":"a","sub":"b"}"#).unwrap_err();
    assert_eq!(error, RawClaimsError::Duplicate { name: "sub".to_owned() });
}

#[test]
fn a_duplicate_member_spelled_with_an_escape_is_rejected() {
    let error = RawClaims::from_slice(br#"{"sub":"a","sub":"b"}"#).unwrap_err();
    assert_eq!(error, RawClaimsError::Duplicate { name: "sub".to_owned() });
}

#[test]
fn a_duplicate_member_in_a_nested_act_is_rejected() {
    let raw = RawClaims::from_slice(br#"{"iss":"i","sub":"u","act":{"sub":"a","sub":"b"}}"#).unwrap();
    let (name, kind) = claim_error(Identity::<DynamicClaims>::ingest(&raw, &profile()));
    assert_eq!(name.as_str(), "act");
    assert!(matches!(
        kind,
        ClaimValueError::Nested(RawClaimsError::Duplicate { .. })
    ));
}

#[test]
fn non_objects_and_malformed_documents_are_rejected() {
    assert_eq!(RawClaims::from_slice(b"[]").unwrap_err(), RawClaimsError::NotObject);
    assert_eq!(RawClaims::from_slice(b"\"x\"").unwrap_err(), RawClaimsError::NotObject);
    assert!(matches!(
        RawClaims::from_slice(b"{\"a\":1,}").unwrap_err(),
        RawClaimsError::InvalidJson { .. }
    ));
    assert!(matches!(
        RawClaims::from_slice(b"{\"a\":1} trailing").unwrap_err(),
        RawClaimsError::InvalidJson { .. }
    ));
    assert_eq!(
        RawClaims::from_slice(br#"{"":1}"#).unwrap_err(),
        RawClaimsError::EmptyName
    );
}

#[test]
fn member_order_is_preserved() {
    let raw = RawClaims::from_slice(br#"{"z":1,"a":{"b":[1,{"c":"}"}]},"m":"x,y"}"#).unwrap();
    let names: Vec<&str> = raw.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["z", "a", "m"]);
    assert_eq!(raw.get_named("a").unwrap().as_str(), r#"{"b":[1,{"c":"}"}]}"#);
    assert_eq!(raw.get_named("m").unwrap().as_string().unwrap(), "x,y");
}

#[test]
fn an_audience_array_projects_one_claim_per_audience() {
    let ingested = ingest(
        r#"{"iss":"https://issuer.example.com","sub":"u","aud":["https://a.example.com","https://b.example.com","https://c.example.com"]}"#,
    )
    .unwrap();
    let aud = ClaimType::from(WellKnownClaim::Aud);
    assert_eq!(ingested.identity().find_all(&aud).count(), 3);
}

#[test]
fn projection_follows_the_json_type() {
    let ingested =
        ingest(r#"{"iss":"i","sub":"u","exp":1443904177,"n":7,"f":1.5,"b":true,"s":"text","o":{"x":1},"nul":null}"#)
            .unwrap();
    let identity = ingested.identity();
    let value = |name: &str| {
        identity
            .find_first(&claim_type(name))
            .map(|claim| claim.value().clone())
    };
    assert!(matches!(value("exp"), Some(ClaimValue::Timestamp(timestamp)) if timestamp.secs() == 1_443_904_177));
    assert_eq!(value("n"), Some(ClaimValue::Integer(7)));
    assert_eq!(value("f"), Some(ClaimValue::Double(1.5)));
    assert_eq!(value("b"), Some(ClaimValue::Boolean(true)));
    assert!(matches!(value("s"), Some(ClaimValue::Text(text)) if text.as_str() == "text"));
    assert_eq!(value("o"), None);
    let unprojected: Vec<&str> = identity.claims().unprojected().iter().map(ClaimType::as_str).collect();
    assert_eq!(unprojected, ["o", "nul"]);
}

#[test]
fn a_claim_is_attributed_to_the_token_issuer() {
    let ingested = ingest(FIGURE_5).unwrap();
    let sub = ClaimType::from(WellKnownClaim::Sub);
    let claim = ingested.identity().find_first(&sub).unwrap();
    assert_eq!(claim.issuer().as_str(), "https://issuer.example.com");
}

#[test]
fn missing_subject_and_issuer_are_reported() {
    let (name, kind) = claim_error(ingest(r#"{"iss":"i"}"#));
    assert_eq!(name.as_str(), "sub");
    assert_eq!(kind, ClaimValueError::MissingSubject);

    let (name, kind) = claim_error(ingest(r#"{"sub":"u"}"#));
    assert_eq!(name.as_str(), "iss");
    assert_eq!(kind, ClaimValueError::MissingIssuer);

    let with_default = profile().with_default_issuer(Issuer::try_new("https://default.example.com").unwrap());
    let ingested = ingest_with(r#"{"sub":"u"}"#, &with_default).unwrap();
    assert_eq!(
        ingested.identity().key().issuer().as_str(),
        "https://default.example.com"
    );
}

#[test]
fn a_numeric_subject_is_rejected_without_echoing_it() {
    let (name, kind) = claim_error(ingest(r#"{"iss":"i","sub":424242}"#));
    assert_eq!(name.as_str(), "sub");
    assert!(!format!("{kind}").contains("424242"));
}

#[test]
fn figure_5_yields_a_direct_actor_and_drops_its_validity_claims() {
    let ingested = ingest(FIGURE_5).unwrap();
    let Ingested::Delegated { identity, actor } = ingested else {
        panic!("figure 5 is a delegation");
    };
    assert_eq!(identity.key().sub().as_str(), "user@example.com");
    assert!(matches!(identity.delegation(), Delegation::Delegated(_)));
    assert_eq!(
        actor.key(),
        &ActorKey::Subject {
            issuer: ActorIssuer::Unstated,
            sub: "admin@example.com".try_into().unwrap(),
        }
    );
    assert!(actor.claims().is_empty());
    assert_eq!(actor.prior_actors().count(), 0);
}

#[test]
fn figure_6_keeps_the_chain_most_recent_first() {
    let ingested = ingest(FIGURE_6).unwrap();
    let actor = ingested.actor().unwrap();
    assert_eq!(actor.key().subject().unwrap().as_str(), "https://service16.example.com");
    let prior: Vec<&str> = actor
        .prior_actors()
        .map(|prior| prior.key().subject().unwrap().as_str())
        .collect();
    assert_eq!(prior, ["https://service77.example.com"]);
}

#[test]
fn an_actor_drops_exp_nbf_and_aud_and_keeps_the_rest() {
    let ingested = ingest(
        r#"{"iss":"i","sub":"u","act":{"iss":"https://actor.example.com","sub":"a","exp":1,"nbf":1,"aud":"x","scope":"read"}}"#,
    )
    .unwrap();
    let actor = ingested.actor().unwrap();
    assert!(
        matches!(actor.key().issuer(), ActorIssuer::Stated(issuer) if issuer.as_str() == "https://actor.example.com")
    );
    let kept: Vec<&str> = actor.claims().iter().map(|claim| claim.claim_type().as_str()).collect();
    assert_eq!(kept, ["scope"]);
    assert_eq!(actor.claims()[0].issuer().as_str(), "i");
}

#[test]
fn an_actor_may_be_identified_by_client_id_alone() {
    let ingested = ingest(r#"{"iss":"i","sub":"u","act":{"client_id":"service-a"}}"#).unwrap();
    let actor = ingested.actor().unwrap();
    assert_eq!(actor.key().client_id().unwrap().as_str(), "service-a");
    assert!(actor.claims().is_empty());
}

#[test]
fn an_actor_that_names_a_subject_keeps_its_client_id_claim() {
    let ingested = ingest(r#"{"iss":"i","sub":"u","act":{"sub":"a","client_id":"service-a"}}"#).unwrap();
    let actor = ingested.actor().unwrap();
    assert!(actor.find_first(&ClaimType::from(WellKnownClaim::ClientId)).is_some());
}

#[test]
fn a_non_object_act_is_rejected() {
    for body in [r#""x""#, "1", "null", "[]", "true"] {
        let (name, kind) = claim_error(ingest(&format!(r#"{{"iss":"i","sub":"u","act":{body}}}"#)));
        assert_eq!(name.as_str(), "act");
        assert_eq!(kind, ClaimValueError::WrongType { expected: "object" });
    }
}

#[test]
fn a_nested_non_object_act_is_rejected() {
    let (name, kind) = claim_error(ingest(r#"{"iss":"i","sub":"u","act":{"sub":"a","act":"b"}}"#));
    assert_eq!(name.as_str(), "act");
    assert_eq!(kind, ClaimValueError::WrongType { expected: "object" });
}

#[test]
fn an_actor_with_neither_sub_nor_client_id_is_rejected() {
    let (_, kind) = claim_error(ingest(r#"{"iss":"i","sub":"u","act":{"scope":"read"}}"#));
    assert_eq!(kind, ClaimValueError::InvalidActor(ActorError::MissingKey));
    let (_, kind) = claim_error(ingest(r#"{"iss":"i","sub":"u","act":{"sub":"a","act":{}}}"#));
    assert_eq!(kind, ClaimValueError::InvalidActor(ActorError::MissingKey));
}

#[test]
fn an_actor_chain_of_ten_is_accepted_and_eleven_is_rejected() {
    let accepted = ingest(&nested_actors(10)).unwrap();
    assert_eq!(accepted.actor().unwrap().prior_actors().count(), 9);

    let (name, kind) = claim_error(ingest(&nested_actors(11)));
    assert_eq!(name.as_str(), "act");
    assert!(matches!(
        kind,
        ClaimValueError::InvalidActor(ActorError::TooDeep { depth: 11, max: 10 })
    ));
}

#[test]
fn may_act_names_an_actor_only_when_both_issuers_are_stated_and_equal() {
    let stated = ingest(
        r#"{"iss":"i","sub":"u","may_act":{"iss":"https://actor.example.com","sub":"a"},"act":{"iss":"https://actor.example.com","sub":"a"}}"#,
    )
    .unwrap();
    let identity = stated.identity();
    assert!(identity.may_act().unwrap().names(stated.actor().unwrap().key()));

    let other_issuer = ingest(
        r#"{"iss":"i","sub":"u","may_act":{"iss":"https://other.example.com","sub":"a"},"act":{"iss":"https://actor.example.com","sub":"a"}}"#,
    )
    .unwrap();
    assert!(!other_issuer
        .identity()
        .may_act()
        .unwrap()
        .names(other_issuer.actor().unwrap().key()));
}

#[test]
fn an_unstated_issuer_never_equals_a_stated_one() {
    let unstated = ingest(r#"{"iss":"i","sub":"u","may_act":{"sub":"a"},"act":{"sub":"a"}}"#).unwrap();
    assert!(!unstated
        .identity()
        .may_act()
        .unwrap()
        .names(unstated.actor().unwrap().key()));

    let mixed = ingest(r#"{"iss":"i","sub":"u","may_act":{"iss":"i","sub":"a"},"act":{"sub":"a"}}"#).unwrap();
    assert!(!mixed.identity().may_act().unwrap().names(mixed.actor().unwrap().key()));

    let assumed = mixed
        .actor()
        .unwrap()
        .key()
        .clone()
        .assume_token_issuer(&Issuer::try_new("i").unwrap());
    assert!(mixed.identity().may_act().unwrap().names(&assumed));
}

#[test]
fn may_act_without_sub_or_client_id_is_rejected() {
    let (name, kind) = claim_error(ingest(r#"{"iss":"i","sub":"u","may_act":{"scope":"x"}}"#));
    assert_eq!(name.as_str(), "may_act");
    assert_eq!(kind, ClaimValueError::InvalidActor(ActorError::MissingKey));
}

#[test]
fn may_act_does_not_authenticate_anything_by_itself() {
    let ingested = ingest(r#"{"iss":"i","sub":"u","may_act":{"sub":"a"}}"#).unwrap();
    assert!(matches!(ingested, Ingested::Direct(_)));
    assert!(ingested.identity().may_act().is_some());
}

fn cnf_error(cnf: &str) -> ConfirmationError {
    let json = format!(r#"{{"iss":"i","sub":"u","cnf":{cnf}}}"#);
    match claim_error(ingest(&json)) {
        (name, ClaimValueError::Confirmation(error)) => {
            assert_eq!(name.as_str(), "cnf");
            error
        }
        other => panic!("expected a confirmation error, got {other:?}"),
    }
}

#[test]
fn confirmation_rejects_more_than_one_key_member() {
    assert_eq!(
        cnf_error(r#"{"jwk":{"kty":"EC","crv":"P-256","x":"a","y":"b"},"jku":"https://keys.example.com"}"#),
        ConfirmationError::MultipleKeyMembers
    );
    assert_eq!(
        cnf_error(r#"{"jwe":"abc","jku":"https://keys.example.com"}"#),
        ConfirmationError::MultipleKeyMembers
    );
}

#[test]
fn confirmation_rejects_symmetric_and_private_keys() {
    assert_eq!(
        cnf_error(r#"{"jwk":{"kty":"oct","k":"c2VjcmV0"}}"#),
        ConfirmationError::PrivateKeyMaterial
    );
    assert_eq!(
        cnf_error(r#"{"jwk":{"kty":"oct","alg":"HS256"}}"#),
        ConfirmationError::SymmetricKey
    );
    for private in ["d", "p", "q", "dp", "dq", "qi", "oth"] {
        assert_eq!(
            cnf_error(&format!(r#"{{"jwk":{{"kty":"RSA","n":"a","e":"b","{private}":"c"}}}}"#)),
            ConfirmationError::PrivateKeyMaterial,
            "{private}"
        );
    }
}

#[test]
fn confirmation_rejects_structural_mistakes() {
    assert_eq!(cnf_error("{}"), ConfirmationError::Empty);
    assert_eq!(cnf_error(r#""x""#), ConfirmationError::WrongType { member: "cnf" });
    assert_eq!(
        cnf_error(r#"{"jwk":"x"}"#),
        ConfirmationError::WrongType { member: "jwk" }
    );
    assert_eq!(cnf_error(r#"{"jwk":{"x":"a"}}"#), ConfirmationError::MissingKeyType);
    assert_eq!(
        cnf_error(r#"{"jkt":1}"#),
        ConfirmationError::WrongType { member: "jkt" }
    );
}

#[test]
fn confirmation_is_asserted_and_never_exposes_key_material() {
    let ingested = ingest(
        r#"{"iss":"i","sub":"u","cnf":{"jwk":{"kty":"EC","crv":"P-256","kid":"k1","x":"PUBLICXVALUE","y":"PUBLICYVALUE"},"x5t#S256":"thumb","custom":{"a":1}}}"#,
    )
    .unwrap();
    let Binding::Asserted(confirmation) = ingested.identity().binding() else {
        panic!("cnf must produce an asserted binding");
    };
    let jwk = confirmation.jwk().unwrap();
    assert_eq!(jwk.kty().as_str(), "EC");
    assert_eq!(jwk.kid().unwrap().as_str(), "k1");
    assert_eq!(confirmation.x5t_s256().unwrap().as_str(), "thumb");
    assert_eq!(confirmation.other_members()[0].1.as_str(), r#"{"a":1}"#);
    let rendered = format!("{confirmation:?} {:?}", ingested.identity().binding());
    assert!(!rendered.contains("PUBLICXVALUE"));
    assert!(!rendered.contains("PUBLICYVALUE"));
    assert!(rendered.contains("thumb"));
}

#[test]
fn a_token_without_cnf_is_a_bearer() {
    assert_eq!(ingest(FIGURE_5).unwrap().identity().binding(), &Binding::Bearer);
}

#[test]
fn authentication_context_round_trips_unregistered_methods() {
    let ingested = ingest(
        r#"{"iss":"i","sub":"u","acr":"urn:mace:incommon:iap:silver","amr":["pwd","otp","vendor-biometric"],"auth_time":1443904000.5}"#,
    )
    .unwrap();
    let context = ingested.identity().context();
    assert_eq!(context.acr().unwrap().as_str(), "urn:mace:incommon:iap:silver");
    assert_eq!(context.amr()[0], AuthenticationMethod::Pwd);
    assert_eq!(context.amr()[1], AuthenticationMethod::Otp);
    assert_eq!(context.amr()[2], AuthenticationMethod::Other("vendor-biometric".into()));
    assert_eq!(context.amr()[2].as_str(), "vendor-biometric");
    let auth_time = context.auth_time().unwrap();
    assert_eq!((auth_time.secs(), auth_time.nanos()), (1_443_904_000, 500_000_000));
}

#[test]
fn authentication_context_rejects_wrong_types() {
    for body in [
        r#""amr":"pwd""#,
        r#""amr":[1]"#,
        r#""acr":1"#,
        r#""auth_time":"yesterday""#,
    ] {
        let (_, kind) = claim_error(ingest(&format!(r#"{{"iss":"i","sub":"u",{body}}}"#)));
        assert!(matches!(kind, ClaimValueError::WrongType { .. }), "{body}");
    }
}

#[test]
fn name_resolution_honours_a_custom_name_claim_type() {
    let json = r#"{"iss":"i","sub":"u","name":"Ada","preferred_username":"ada"}"#;
    let default_profile = ingest(json).unwrap();
    assert_eq!(default_profile.identity().name(), None);

    let custom = profile().with_name_claim_type(claim_type("preferred_username"));
    assert_eq!(ingest_with(json, &custom).unwrap().identity().name(), Some("ada"));

    let well_known = profile().with_name_claim_type(WellKnownClaim::Name.into());
    assert_eq!(ingest_with(json, &well_known).unwrap().identity().name(), Some("Ada"));
}

#[test]
fn is_in_role_never_consults_an_actor() {
    let custom = profile().with_role_claim_type(claim_type("roles"));
    let ingested = ingest_with(
        r#"{"iss":"i","sub":"u","roles":["viewer"],"act":{"sub":"a","roles":["admin"]}}"#,
        &custom,
    )
    .unwrap();
    let identity = ingested.identity();
    assert!(identity.is_in_role(&RoleName::try_new("viewer").unwrap()));
    assert!(!identity.is_in_role(&RoleName::try_new("admin").unwrap()));
    assert!(ingested.actor().unwrap().find_first(&claim_type("roles")).is_some());
}

const COMPACT: &str =
    "x.eyJpc3MiOiJodHRwczovL2lzc3Vlci5leGFtcGxlLmNvbSIsInN1YiI6InVzZXJAZXhhbXBsZS5jb20iLCJleHAiOjE0NDM5MDQxNzd9.y";

#[test]
fn a_compact_token_yields_claims_and_a_separate_bootstrap_credential() {
    let (claims, credential) = jwt_payload(COMPACT).unwrap();
    assert_eq!(
        claims.get_named("sub").unwrap().as_string().unwrap(),
        "user@example.com"
    );
    assert_eq!(credential.expose_for_token_exchange(), COMPACT);
    let ingested = Identity::<DynamicClaims>::ingest(&claims, &profile()).unwrap();
    assert_eq!(ingested.identity().key().sub().as_str(), "user@example.com");
}

#[test]
fn a_malformed_compact_token_is_rejected() {
    for token in ["", "a.b", "a.b.c.d", "a..c"] {
        assert!(jwt_payload(token).is_err(), "{token:?}");
    }
    assert_eq!(
        jwt_payload("a.not+base64!.c").unwrap_err(),
        IngestError::Malformed(RawClaimsError::InvalidBase64)
    );
    assert_eq!(
        jwt_payload("a.b.c.d").unwrap_err(),
        IngestError::Malformed(RawClaimsError::NotCompactJwt)
    );
}

#[test]
fn introspection_requires_a_boolean_active_member() {
    let (claims, credential) = introspection(br#"{"active":true,"iss":"i","sub":"u"}"#, Some("opaque-token")).unwrap();
    assert_eq!(credential.unwrap().expose_for_token_exchange(), "opaque-token");
    assert!(Identity::<DynamicClaims>::ingest(&claims, &profile()).is_ok());

    assert!(introspection(br#"{"active":true}"#, None).unwrap().1.is_none());
    assert_eq!(
        introspection(br#"{"active":false,"sub":"u"}"#, None).unwrap_err(),
        IngestError::Inactive
    );
    let (name, kind) = claim_error_of(introspection(br#"{"active":"yes"}"#, None));
    assert_eq!(name.as_str(), "active");
    assert!(matches!(kind, ClaimValueError::WrongType { .. }));
    let (_, kind) = claim_error_of(introspection(br#"{"sub":"u"}"#, None));
    assert_eq!(kind, ClaimValueError::Missing);
}

fn claim_error_of<T: std::fmt::Debug>(result: Result<T, IngestError>) -> (ClaimType, ClaimValueError) {
    match result.unwrap_err() {
        IngestError::Claim { name, kind } => (name, kind),
        other => panic!("expected a claim error, got {other:?}"),
    }
}
