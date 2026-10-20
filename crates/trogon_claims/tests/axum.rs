#![cfg(all(feature = "axum", feature = "json"))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::Request;
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{from_fn, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tower::ServiceExt;
use trogon_claims::axum::{
    insert_credential_outcome, insert_presented_proof, insert_shared_principal, CredentialOutcome, Masked,
    PresentedProof, PrincipalOrNotFound, RejectReason, StepUpRequirement,
};
use trogon_claims::{
    principal_kind, AuthenticatedPrincipal, AuthenticationContext, AuthenticationType, ClaimText, CurrentPrincipal,
    Delegated, Dpop, DynamicClaims, Identity, IngestProfile, Kind, NumericDate, Principal, PrincipalKind, RawClaims,
    Refined, SenderConstrained, Unclassified,
};

struct Person;

impl Kind for Person {
    const KIND: &'static PrincipalKind = &principal_kind!("person");
}

struct Service;

impl Kind for Service {
    const KIND: &'static PrincipalKind = &principal_kind!("service");
}

const PLAIN: &str = r#"{"iss":"https://issuer.example.com","sub":"user-1"}"#;
const DELEGATED: &str = r#"{"iss":"https://issuer.example.com","sub":"user-1","act":{"sub":"agent-1"}}"#;
const DPOP: &str = r#"{"iss":"https://issuer.example.com","sub":"user-1","cnf":{"jkt":"thumb-a"}}"#;
const WEAK_AUTH: &str = r#"{"iss":"https://issuer.example.com","sub":"user-1","acr":"basic","auth_time":1000}"#;
const STRONG_AUTH: &str = r#"{"iss":"https://issuer.example.com","sub":"user-1","acr":"mfa","auth_time":1000}"#;

fn ingest(body: &str) -> Identity<DynamicClaims> {
    let raw = RawClaims::from_slice(body.as_bytes()).unwrap();
    let profile = IngestProfile::new(AuthenticationType::try_new("jwt").unwrap());
    Identity::<DynamicClaims>::ingest(&raw, &profile)
        .unwrap()
        .into_identity()
}

fn principal_for(token: &str) -> Option<Principal> {
    let body = match token {
        "plain" => PLAIN,
        "delegated" => DELEGATED,
        "dpop" => DPOP,
        "weak" => WEAK_AUTH,
        "strong" => STRONG_AUTH,
        _ => return None,
    };
    Some(Principal::new(ingest(body)))
}

fn classified(kind: PrincipalKind) -> Principal {
    Principal::new(ingest(PLAIN).with_kind(kind))
}

async fn attach(mut request: Request, next: Next) -> Response {
    let token = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned);
    let proof = request
        .headers()
        .get("x-proof")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let principal = match token.as_deref() {
        None => {
            insert_credential_outcome(&mut request, CredentialOutcome::Absent);
            Principal::anonymous()
        }
        Some("expired") => {
            insert_credential_outcome(&mut request, CredentialOutcome::Rejected(RejectReason::Expired));
            Principal::anonymous()
        }
        Some("person") => classified(Person::KIND.clone()),
        Some("service") => classified(Service::KIND.clone()),
        Some(other) => principal_for(other).unwrap_or_else(Principal::anonymous),
    };
    if let Some(proof) = proof {
        insert_presented_proof(&mut request, PresentedProof::<Dpop>::new(proof));
    }
    insert_shared_principal(&mut request, Arc::new(principal));
    next.run(request).await
}

async fn any_principal(current: CurrentPrincipal) -> String {
    current.is_authenticated().to_string()
}

async fn authenticated(_: AuthenticatedPrincipal) -> &'static str {
    "ok"
}

async fn person(_: AuthenticatedPrincipal<DynamicClaims, Refined<Person>>) -> &'static str {
    "person"
}

async fn delegated(_: AuthenticatedPrincipal<DynamicClaims, Refined<Unclassified, Delegated>>) -> &'static str {
    "delegated"
}

async fn dpop(
    principal: AuthenticatedPrincipal<
        DynamicClaims,
        Refined<Unclassified, trogon_claims::AnyDelegation, SenderConstrained<Dpop>>,
    >,
) -> String {
    principal.binding().thumbprint().to_owned()
}

async fn hidden(_: PrincipalOrNotFound<DynamicClaims>) -> &'static str {
    "hidden"
}

async fn masked(_: Masked<AuthenticatedPrincipal>) -> &'static str {
    "masked"
}

async fn sensitive(principal: AuthenticatedPrincipal) -> Response {
    let requirement = StepUpRequirement::try_new(vec![ClaimText::from("mfa")], Some(Duration::from_secs(300))).unwrap();
    let now = NumericDate::try_new(1100, 0).unwrap();
    match requirement.check(principal.context(), now) {
        Ok(()) => "sensitive".into_response(),
        Err(required) => required.into_response(),
    }
}

fn routes() -> Router {
    Router::new()
        .route("/any", get(any_principal))
        .route("/me", get(authenticated))
        .route("/person", get(person))
        .route("/delegated", get(delegated))
        .route("/dpop", get(dpop))
        .route("/hidden", get(hidden))
        .route("/masked", get(masked))
        .route("/sensitive", get(sensitive))
}

fn router() -> Router {
    routes().layer(from_fn(attach))
}

async fn call(router: Router, path: &str, headers: &[(&str, &str)]) -> (StatusCode, HeaderMap) {
    let mut builder = Request::builder().uri(path);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router.oneshot(builder.body(Body::empty()).unwrap()).await.unwrap();
    (response.status(), response.headers().clone())
}

fn challenge(headers: &HeaderMap) -> &str {
    headers.get(WWW_AUTHENTICATE).unwrap().to_str().unwrap()
}

#[tokio::test]
async fn a_request_without_credentials_gets_a_bare_bearer_challenge() {
    let (status, headers) = call(router(), "/me", &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(challenge(&headers), "Bearer");
    assert!(!challenge(&headers).contains("error"));
}

#[tokio::test]
async fn a_rejected_token_gets_the_invalid_token_challenge() {
    let (status, headers) = call(router(), "/me", &[("authorization", "Bearer expired")]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(challenge(&headers), "Bearer error=\"invalid_token\"");
    assert!(!challenge(&headers).contains("expired"));
}

#[tokio::test]
async fn a_missing_outcome_extension_is_treated_as_no_credentials() {
    async fn only_anonymous(mut request: Request, next: Next) -> Response {
        insert_shared_principal(&mut request, Arc::new(Principal::<DynamicClaims>::anonymous()));
        next.run(request).await
    }
    let (status, headers) = call(routes().layer(from_fn(only_anonymous)), "/me", &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(challenge(&headers), "Bearer");
}

#[tokio::test]
async fn an_authenticated_request_reaches_the_handler() {
    let (status, headers) = call(router(), "/me", &[("authorization", "Bearer plain")]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get(WWW_AUTHENTICATE).is_none());
}

#[tokio::test]
async fn a_missing_layer_is_a_server_error_not_an_authentication_failure() {
    let (status, headers) = call(routes(), "/me", &[]).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(headers.get(WWW_AUTHENTICATE).is_none());
    let (status, _) = call(routes(), "/any", &[]).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn the_current_principal_extractor_accepts_anonymous() {
    let (status, _) = call(router(), "/any", &[]).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_kind_mismatch_is_forbidden_without_a_challenge() {
    let (status, _) = call(router(), "/person", &[("authorization", "Bearer person")]).await;
    assert_eq!(status, StatusCode::OK);
    let (status, headers) = call(router(), "/person", &[("authorization", "Bearer service")]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(headers.get(WWW_AUTHENTICATE).is_none());
    let (status, _) = call(router(), "/person", &[("authorization", "Bearer plain")]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_delegation_requirement_is_enforced() {
    let (status, _) = call(router(), "/delegated", &[("authorization", "Bearer delegated")]).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(router(), "/delegated", &[("authorization", "Bearer plain")]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_dpop_route_requires_a_matching_proof() {
    let (status, headers) = call(router(), "/dpop", &[("authorization", "Bearer dpop")]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(challenge(&headers), "DPoP");

    let (status, headers) = call(
        router(),
        "/dpop",
        &[("authorization", "Bearer dpop"), ("x-proof", "thumb-z")],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(challenge(&headers), "DPoP error=\"invalid_token\"");

    let (status, _) = call(
        router(),
        "/dpop",
        &[("authorization", "Bearer dpop"), ("x-proof", "thumb-a")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, headers) = call(
        router(),
        "/dpop",
        &[("authorization", "Bearer plain"), ("x-proof", "thumb-a")],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(challenge(&headers), "DPoP error=\"invalid_token\"");
}

#[tokio::test]
async fn hidden_routes_answer_not_found_instead_of_challenging() {
    let (status, headers) = call(router(), "/hidden", &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(headers.get(WWW_AUTHENTICATE).is_none());
    let (status, _) = call(router(), "/hidden", &[("authorization", "Bearer plain")]).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(routes(), "/hidden", &[]).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn the_masked_wrapper_hides_any_rejection() {
    let (status, headers) = call(router(), "/masked", &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(headers.get(WWW_AUTHENTICATE).is_none());
    let (status, _) = call(router(), "/masked", &[("authorization", "Bearer plain")]).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_step_up_refusal_names_the_required_assurance() {
    let (status, headers) = call(router(), "/sensitive", &[("authorization", "Bearer weak")]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let value = challenge(&headers);
    assert!(value.starts_with("Bearer error=\"insufficient_user_authentication\""));
    assert!(value.contains("acr_values=\"mfa\""));
    assert!(value.contains("max_age=\"300\""));

    let (status, _) = call(router(), "/sensitive", &[("authorization", "Bearer strong")]).await;
    assert_eq!(status, StatusCode::OK);
}

#[test]
fn step_up_checks_age_and_assurance_independently() {
    let now = NumericDate::try_new(2000, 0).unwrap();
    let context = |acr: Option<&'static str>, auth_time: Option<i64>| {
        AuthenticationContext::new(
            acr.map(ClaimText::from),
            Vec::new(),
            auth_time.map(|secs| NumericDate::try_new(secs, 0).unwrap()),
        )
    };
    let acr_only = StepUpRequirement::try_new(vec![ClaimText::from("mfa")], None).unwrap();
    assert!(acr_only.check(&context(Some("mfa"), None), now).is_ok());
    assert!(acr_only.check(&context(Some("pwd"), Some(1990)), now).is_err());
    assert!(acr_only.check(&context(None, Some(1990)), now).is_err());

    let age_only = StepUpRequirement::try_new(Vec::new(), Some(Duration::from_secs(60))).unwrap();
    assert!(age_only.check(&context(None, Some(1950)), now).is_ok());
    assert!(age_only.check(&context(None, Some(1900)), now).is_err());
    assert!(age_only.check(&context(None, None), now).is_err());
}

#[test]
fn step_up_requirements_reject_empty_and_unsafe_values() {
    assert!(StepUpRequirement::try_new(Vec::new(), None).is_err());
    for bad in ["", "a b", "a\"b", "a\\b", "caf\u{e9}"] {
        assert!(StepUpRequirement::try_new(vec![ClaimText::from(bad.to_owned())], None).is_err());
    }
}
