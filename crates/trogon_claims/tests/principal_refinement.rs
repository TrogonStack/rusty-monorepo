#![cfg(feature = "json")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use trogon_claims::{
    AnyBinding, AnyDelegation, AuthenticatedPrincipal, AuthenticationType, Binding, BindingError, BindingMethod,
    Classified, CurrentPrincipal, Delegated, Dpop, DynamicClaims, Identity, IngestProfile, Ingested, Kind, MutualTls,
    NameClaimType, Principal, PrincipalKind, RawClaims, Refined, Refinement, RefinementError, RoleClaimType,
    SenderConstrained, Unclassified, WellKnownClaim,
};

struct Person;

impl Kind for Person {
    const KIND: &'static PrincipalKind = &PrincipalKind::from_static_unchecked("person");
}

struct Service;

impl Kind for Service {
    const KIND: &'static PrincipalKind = &PrincipalKind::from_static_unchecked("service");
}

fn ingest(body: &str) -> Ingested<DynamicClaims> {
    let raw = RawClaims::from_slice(body.as_bytes()).unwrap();
    let profile = IngestProfile::new(
        AuthenticationType::try_new("jwt").unwrap(),
        NameClaimType::new(WellKnownClaim::PreferredUsername.into()),
        RoleClaimType::new(WellKnownClaim::Roles.into()),
    );
    Identity::<DynamicClaims>::ingest(&raw, &profile).unwrap()
}

fn principal(body: &str) -> Arc<Principal> {
    Arc::new(ingest(body).into_principal())
}

fn classified(body: &str, kind: PrincipalKind) -> Arc<Principal> {
    let identity = ingest(body).into_identity().try_with_kind(kind).unwrap();
    Arc::new(Principal::new(identity))
}

const PLAIN: &str = r#"{"iss":"https://issuer.example.com","sub":"user-1"}"#;
const DELEGATED: &str = r#"{"iss":"https://issuer.example.com","sub":"user-1","act":{"sub":"agent-1"}}"#;
const DPOP: &str = r#"{"iss":"https://issuer.example.com","sub":"user-1","cnf":{"jkt":"thumb-a"}}"#;
const MTLS: &str = r#"{"iss":"https://issuer.example.com","sub":"user-1","cnf":{"x5t#S256":"thumb-b"}}"#;

#[test]
fn an_ingested_value_converts_into_a_one_identity_principal() {
    let from_method = ingest(PLAIN).into_principal();
    let from_trait: Principal = ingest(DELEGATED).into();
    assert_eq!(from_method.identities().len(), 1);
    assert!(from_method.is_authenticated());
    assert!(from_trait.is_authenticated());
}

#[test]
fn an_ingested_actor_is_always_the_identity_actor() {
    for body in [PLAIN, DELEGATED] {
        let ingested = ingest(body);
        let actor = ingested.actor();
        let identity_actor = ingested.identity().actor().map(Arc::as_ref);
        assert_eq!(actor, identity_actor);
        if let (Some(actor), Some(identity_actor)) = (actor, identity_actor) {
            assert!(std::ptr::eq(actor, identity_actor));
        }
        let principal = ingested.into_principal();
        assert_eq!(
            principal.primary().as_authenticated().unwrap().actor().is_some(),
            body == DELEGATED
        );
    }
}

#[test]
fn an_anonymous_principal_cannot_become_authenticated() {
    let current = CurrentPrincipal::from(Principal::<DynamicClaims>::anonymous());
    assert!(!current.is_authenticated());
    assert!(current.try_authenticated().is_err());
    assert!(AuthenticatedPrincipal::try_new(Arc::new(Principal::<DynamicClaims>::anonymous())).is_err());
}

#[test]
fn the_wrapper_holds_the_resolved_primary() {
    let wrapper = CurrentPrincipal::from(principal(PLAIN)).try_authenticated().unwrap();
    assert_eq!(wrapper.authentication_type().as_str(), "jwt");
    assert_eq!(wrapper.key().sub().as_str(), "user-1");
    assert_eq!(wrapper.authenticated_identities().count(), 1);
}

#[test]
fn kind_refinement_compares_the_application_classification() {
    let person = AuthenticatedPrincipal::try_new(classified(PLAIN, Person::KIND.clone())).unwrap();
    let refined: AuthenticatedPrincipal<DynamicClaims, Refined<Person>> = person.clone().try_kind::<Person>().unwrap();
    assert_eq!(refined.kind().as_str(), "person");
    assert_eq!(person.try_kind::<Service>().unwrap_err(), RefinementError::KindMismatch);

    let unclassified = AuthenticatedPrincipal::try_new(principal(PLAIN)).unwrap();
    assert_eq!(
        unclassified.clone().try_kind::<Person>().unwrap_err(),
        RefinementError::Unclassified
    );
    assert_eq!(
        unclassified.try_classified().unwrap_err(),
        RefinementError::Unclassified
    );
}

#[test]
fn classified_refinement_carries_the_runtime_kind() {
    let wrapper = AuthenticatedPrincipal::try_new(classified(PLAIN, Service::KIND.clone())).unwrap();
    let refined: AuthenticatedPrincipal<DynamicClaims, Refined<Classified>> = wrapper.try_classified().unwrap();
    assert_eq!(refined.kind().as_str(), "service");
}

#[test]
fn delegation_refinement_makes_the_actor_infallible() {
    let wrapper = AuthenticatedPrincipal::try_new(principal(DELEGATED)).unwrap();
    let delegated: AuthenticatedPrincipal<DynamicClaims, Refined<Unclassified, Delegated>> =
        wrapper.clone().try_delegated().unwrap();
    assert_eq!(delegated.current_actor().key().subject().unwrap().as_str(), "agent-1");
    assert_eq!(wrapper.try_direct().unwrap_err(), RefinementError::NotDirect);

    let direct = AuthenticatedPrincipal::try_new(principal(PLAIN)).unwrap();
    assert_eq!(
        direct.clone().try_delegated().unwrap_err(),
        RefinementError::NotDelegated
    );
    assert!(direct.try_direct().is_ok());
}

fn audit<C: trogon_claims::ClaimSet, R: Refinement<Delegation = Delegated>>(
    principal: &AuthenticatedPrincipal<C, R>,
) -> String {
    principal.current_actor().key().subject().unwrap().as_str().to_owned()
}

#[test]
fn a_function_can_constrain_one_axis_and_leave_the_others_free() {
    let delegated = AuthenticatedPrincipal::try_new(principal(DELEGATED))
        .unwrap()
        .try_delegated()
        .unwrap();
    assert_eq!(audit(&delegated), "agent-1");
}

#[test]
fn dpop_binding_compares_the_presented_thumbprint_exactly() {
    let wrapper = AuthenticatedPrincipal::try_new(principal(DPOP)).unwrap();
    assert!(matches!(wrapper.binding(), Binding::Asserted(_)));
    assert_eq!(
        wrapper.clone().try_bind::<Dpop>("thumb-x").unwrap_err(),
        BindingError::Mismatch(BindingMethod::Dpop)
    );
    assert_eq!(
        wrapper.clone().try_bind::<MutualTls>("thumb-a").unwrap_err(),
        BindingError::MemberMissing(BindingMethod::MutualTls)
    );
    let bound: AuthenticatedPrincipal<DynamicClaims, Refined<Unclassified, AnyDelegation, SenderConstrained<Dpop>>> =
        wrapper.try_bind::<Dpop>("thumb-a").unwrap();
    assert_eq!(bound.binding().thumbprint(), "thumb-a");
    assert_eq!(bound.binding().verified().method(), BindingMethod::Dpop);
}

#[test]
fn mutual_tls_binding_reads_the_certificate_thumbprint() {
    let wrapper = AuthenticatedPrincipal::try_new(principal(MTLS)).unwrap();
    let bound = wrapper.try_bind::<MutualTls>("thumb-b").unwrap();
    assert_eq!(bound.binding().thumbprint(), "thumb-b");
}

#[test]
fn a_bearer_token_cannot_be_bound() {
    let wrapper = AuthenticatedPrincipal::try_new(principal(PLAIN)).unwrap();
    assert_eq!(
        wrapper.try_bind::<Dpop>("thumb-a").unwrap_err(),
        BindingError::NotAsserted
    );
}

#[test]
fn axes_compose_in_any_order() {
    let wrapper = AuthenticatedPrincipal::try_new(principal(
        r#"{"iss":"https://issuer.example.com","sub":"user-1","act":{"sub":"agent-1"},"cnf":{"jkt":"thumb-a"}}"#,
    ))
    .unwrap();
    let both = wrapper.try_delegated().unwrap().try_bind::<Dpop>("thumb-a").unwrap();
    assert_eq!(both.current_actor().key().subject().unwrap().as_str(), "agent-1");
    assert_eq!(both.binding().thumbprint(), "thumb-a");
}

#[test]
fn a_verified_binding_can_be_recorded_on_the_stored_identity() {
    let identity = ingest(DPOP).into_identity();
    let verified = identity
        .as_authenticated()
        .unwrap()
        .verify_binding::<Dpop>("thumb-a")
        .unwrap();
    let stored = identity.try_with_verified_binding(verified).unwrap();
    let binding = stored.as_authenticated().unwrap().binding();
    assert!(matches!(binding, Binding::Verified(_)));
    assert!(stored
        .as_authenticated()
        .unwrap()
        .verify_binding::<Dpop>("thumb-a")
        .is_ok());
    assert_eq!(
        stored
            .as_authenticated()
            .unwrap()
            .verify_binding::<Dpop>("thumb-z")
            .unwrap_err(),
        BindingError::Mismatch(BindingMethod::Dpop)
    );
}

#[test]
fn a_verified_binding_cannot_replace_another_identity_assertion() {
    let source = ingest(DPOP).into_identity();
    let verified = source
        .as_authenticated()
        .unwrap()
        .verify_binding::<Dpop>("thumb-a")
        .unwrap();
    let target =
        ingest(r#"{"iss":"https://issuer.example.com","sub":"user-2","cnf":{"jkt":"thumb-z"}}"#).into_identity();
    assert_eq!(
        target.try_with_verified_binding(verified).unwrap_err(),
        BindingError::Mismatch(BindingMethod::Dpop)
    );
}

#[test]
fn stored_binding_evidence_cannot_change_its_method_or_bind_an_unasserted_identity() {
    let source = ingest(DPOP).into_identity();
    let verified = source
        .as_authenticated()
        .unwrap()
        .verify_binding::<Dpop>("thumb-a")
        .unwrap();
    assert_eq!(
        ingest(MTLS)
            .into_identity()
            .try_with_verified_binding(verified.clone())
            .unwrap_err(),
        BindingError::MemberMissing(BindingMethod::Dpop)
    );
    assert_eq!(
        ingest(PLAIN)
            .into_identity()
            .try_with_verified_binding(verified.clone())
            .unwrap_err(),
        BindingError::NotAsserted
    );
    let anonymous: Identity = Identity::anonymous();
    assert_eq!(
        anonymous.try_with_verified_binding(verified).unwrap_err(),
        BindingError::NotAsserted
    );
}

#[test]
fn the_default_refinement_spells_as_plain_authenticated_principal() {
    let _: AuthenticatedPrincipal = AuthenticatedPrincipal::<
        DynamicClaims,
        Refined<Unclassified, AnyDelegation, AnyBinding>,
    >::try_new(principal(PLAIN))
    .unwrap();
}
