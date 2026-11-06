//! Validated claim, identity and principal value objects.
//!
//! Represent claims, identities and principals as immutable Rust values, with
//! explicit name/role selection, typed claim interpretation and identity
//! provenance. Optional SpiceDB authorization subjects use a separate validated
//! mapping; the core identity key keeps its original issuer and subject.
//! The caller verifies credentials before constructing authenticated identity
//! data; this crate checks representation and refinement state, never a token
//! signature or resource access decision.
//!
//! .NET provides an object-model reference; JWT, OpenID Connect and OAuth specifications
//! define the protocol semantics. [`WellKnownClaim`] links each JWT/OIDC claim
//! to its source, while [`CurrentActor`], [`AuthorizedActor`], [`Confirmation`]
//! and [`Binding`] cite the delegation and proof-of-possession rules. These are
//! references for interpreting the values, not a token-validation profile.
//!
//! # Build a principal
//!
//! ```
//! use trogon_claims::{
//!     AuthenticationType, Claim, ClaimText, ClaimValue, IdentityBuilder, Issuer, NameClaimType,
//!     Principal, RoleClaimType, WellKnownClaim,
//! };
//!
//! let issuer = Issuer::try_new("https://issuer.example.com")?;
//! let subject = Claim::builder(WellKnownClaim::Sub.into(), ClaimValue::Text(ClaimText::from("user-1")))
//!     .issuer(issuer.clone())
//!     .build();
//! let identity = IdentityBuilder::new(
//!     AuthenticationType::try_new("jwt")?,
//!     NameClaimType::new(WellKnownClaim::Sub.into()),
//!     RoleClaimType::new(WellKnownClaim::Roles.into()),
//! )
//!     .with_default_issuer(issuer)
//!     .push(subject)
//!     .try_build()?;
//! let principal = Principal::new(identity);
//!
//! assert!(principal.is_authenticated());
//! let sourced = principal.find_first(&WellKnownClaim::Sub.into()).ok_or("subject missing")?;
//! assert_eq!(sourced.identity().key().sub().as_str(), "user-1");
//! assert_eq!(sourced.claim().claim_type(), &WellKnownClaim::Sub.into());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! With the `json` feature, the `ingest` module turns a verified token payload into the
//! same types in one step. Parsing raw JSON does not establish credential trust.
//! Manual construction requires singular textual `iss` and `sub` claims; an
//! explicit `with_default_issuer` supplies an issuer only when `iss` is absent.
//! Malformed or duplicate issuer/subject claims are rejected.
//!
//! # Compose identities deliberately
//!
//! ```
//! use trogon_claims::{Identity, PrimaryIdentityIndex, Principal, PrincipalBuildError, PrincipalBuilder};
//!
//! fn compose(first: Identity, second: Identity) -> Result<Principal, PrincipalBuildError> {
//!     PrincipalBuilder::new(first)
//!         .push(second)
//!         .primary(PrimaryIdentityIndex::new(0))
//!         .try_build()
//! }
//! ```
//!
//! Selecting an anonymous or out-of-range primary fails. Querying a principal
//! searches its authenticated identities in insertion order and returns
//! [`SourcedClaim`], which retains both the claim and its originating identity.
//! Appending an identity does not change the primary.
//!
//! # Interpret standard claims
//!
//! [`DynamicClaims::standard`] provides typed accessors for standard assertions.
//! `Ok(None)` means absent, `Ok(Some(value))` means a supported valid
//! representation, and [`ClaimLookupError`] distinguishes malformed or
//! ambiguous values. Requiredness and token acceptance remain caller policy.
//! Scopes, roles and groups have separate value types. Custom claims remain
//! accessible through a validated [`ClaimType`].
//!
//! # Coming from .NET
//!
//! | .NET | Here | Note |
//! |---|---|---|
//! | `ClaimsPrincipal` | [`Principal`] | The claim-set parameter defaults to [`DynamicClaims`], so `Principal` alone works |
//! | `ClaimsIdentity` | [`Identity`] | An enum: `Anonymous` is a variant, not an empty string |
//! | `IsAuthenticated` | [`Identity::is_authenticated`], or a `match` | Not an emptiness check on a display string |
//! | `Claim.Value` and `Claim.ValueType` | [`ClaimValue`] | One field whose type is the value type, so the two cannot disagree |
//! | `AddClaim`, `AddIdentity` | [`IdentityBuilder`], [`PrincipalBuilder`], [`Principal::with_identity`] | Builders, then immutable values; composition preserves an explicitly selected primary |
//! | `FindFirst(string)` | [`Principal::find_first`] | Takes [`ClaimType`] and returns [`SourcedClaim`] with its identity provenance |
//! | `FindAll(string)` | [`Principal::find_all`] | Same; queries iterate authenticated identities in insertion order |
//! | `IsInRole` | [`Principal::is_in_role`] | Same semantics, including the per-identity role claim type |
//! | `ClaimTypes.*` | [`WellKnownClaim`] | Short names and the `WS-*` URIs, matched exactly |
//! | `GenericPrincipal` | none | One principal type, one storage |
//! | `IClaimsTransformation` | a plain function | Values are immutable, so a transformation returns a new one |
//!
//! [`IdentityBuilder`] and [`IngestProfile`] require explicit [`NameClaimType`]
//! and [`RoleClaimType`] arguments. No name or role claim type is selected by
//! default, and JWT/OIDC names are not rewritten to WS-* URIs. Each selector
//! uses one type without fallback; the distinct types prevent swapping them.
//! The authentication key remains issuer plus subject, independently of the
//! selected principal name.
//!
//! [MicroProfile JWT 2.1](https://download.eclipse.org/microprofile/microprofile-jwt-auth-2.1/microprofile-jwt-auth-spec-2.1.html)
//! offers a related JVM model through `JsonWebToken` and its `Claims` enum. Its
//! `upn`, `preferred_username`, `sub` name fallback and `groups` role convention
//! are profile choices that this crate does not apply automatically.
//!
//! # Where this departs from .NET
//!
//! - **Anonymous is a variant.** [`Identity::Anonymous`] carries nothing, so an
//!   unauthenticated request cannot be mistaken for an identity with an empty
//!   name. Whether a credential was missing or refused is request metadata, not
//!   part of the identity.
//! - **A claim value has a type, and the type is the value type.** There is no
//!   free-form `ValueType` string to drift from the value. [`ClaimValue`] has no
//!   null arm.
//! - **Claim types are explicit values.** Lookups take [`ClaimType`]. Known
//!   names match exactly; custom non-empty names remain available. Custom-name
//!   validation does not detect a misspelling of a standard claim.
//! - **Roles are a claim query, not a decision.** [`Principal::is_in_role`]
//!   reports what the token asserted. Authorization belongs to a permission
//!   check against a resource.
//! - **Queries read authenticated identities only.** [`Principal::claims`],
//!   [`Principal::find_first`], [`Principal::find_all`] and
//!   [`Principal::is_in_role`] skip anonymous identities.
//! - **Authentication is a type.** [`AuthenticatedPrincipal`] is a distinct
//!   wrapper that cannot be built from an anonymous principal. It deliberately
//!   has no `Deref` to [`Principal`]. It proves that a caller-supplied principal
//!   has an authenticated primary; credential verification remains the caller's
//!   responsibility.
//! - **Refinements are consuming and typed.** `try_kind`, `try_classified`,
//!   `try_delegated`, `try_direct` and `try_bind` return a wrapper whose type
//!   records what was checked, and a binding is only ever verified through the
//!   proof the verifying layer presents.
//! - **Delegation is modelled.** The `act` chain is a bounded, acyclic value
//!   ([`CurrentActor`], [`PriorActors`]), and an actor key stays unstated until
//!   the caller supplies an issuer, because an unstated issuer must never equal a
//!   stated one.
//! - **No ambient identity.** Nothing is stored in thread-local or process-global
//!   state. A principal is passed explicitly.
//! - **Primary selection is explicit.** A multi-identity [`PrincipalBuilder`]
//!   requires [`PrimaryIdentityIndex`]. Appending through
//!   [`Principal::with_identity`] preserves the current primary, including an
//!   anonymous primary; [`Principal::try_with_primary`] validates a deliberate
//!   change to another authenticated identity.
//! - **Bootstrap material never travels with an identity.** The raw credential
//!   returned beside the claims is a [`BootstrapCredential`], which is redacted
//!   in `Debug`, has no `Clone` and no serde, and is zeroized on drop.
//! - **Sensitive input is not echoed.** Errors from ingest and from the
//!   authorization projection never carry token or claim content.
//! - **No async, no I/O.** Every accessor is synchronous. The only `async fn` is
//!   the extractor implementation behind the `axum` feature, which comes from
//!   `axum-core` and brings no runtime.
//!
//! # Features
//!
//! | Feature | Adds | Extra dependencies |
//! |---|---|---|
//! | none | Value objects, claims, identities, principals and refinements | `thiserror`, `zeroize`, `trogon_claims_syntax` |
//! | `macros` | `principal_kind!`; backend literal constructors also require `spicedb` | `trogon_claims_macros` |
//! | `json` | `ingest`, `RawClaims`, `RawJson`, `IngestProfile`-driven identity construction | `serde_json`, `fluent-uri` |
//! | `serde` | Reserved for session serialization. It carries no code yet | `serde` |
//! | `spicedb` | `ObjectId`, `ObjectType`, backend subject mapping and accessors, object/subject references, check requests, caveat conversion and tri-state outcomes | `prost`, `prost-types` |
//! | `axum` | The `axum` module: extractors, `WWW-Authenticate` challenges, step-up responses | `axum-core`, `http` |
//!
//! The dependency budget for each feature is enforced in CI by
//! `.config/mise/tasks/claims/budget`.
//! No optional feature is enabled by default. Without `macros`, validated
//! runtime and const constructors remain available; public literal macros and
//! their procedural-macro dependency are absent.
//!
//! # Consuming this crate
//!
//! The crates are not published to crates.io. `publish = false` is set on every
//! crate in the family and none of them is listed in the release configuration.
//! Depend on them by git revision and pin the revision:
//!
//! ```toml
//! [dependencies]
//! trogon_claims = { git = "https://github.com/TrogonStack/rusty-monorepo", rev = "<commit>", features = ["json"] }
//! ```
//!
//! # What is deliberately absent
//!
//! There is no `canonicalize`, no `Deref<Target = str>` on a value object, no
//! derived `Deserialize` that bypasses `try_new`, and no conversion from a
//! permission outcome to `bool`: a conditional outcome is neither a grant nor a
//! denial, so callers must match every arm. The constraints exist to keep each
//! proof of validity impossible to forge.

mod actor_error;
mod actor_issuer;
mod actor_key;
#[cfg(feature = "spicedb")]
mod actor_subject_error;
mod audience;
mod authenticated_principal;
mod authentication_context;
mod authentication_method;
mod authentication_type;
#[cfg(feature = "spicedb")]
mod authorization_subject;
mod authorized_actor;
#[cfg(feature = "axum")]
pub mod axum;
#[cfg(feature = "spicedb")]
mod base64_standard;
#[cfg(any(feature = "json", feature = "spicedb"))]
mod base64url;
mod binding;
mod bootstrap_credential;
#[cfg(feature = "spicedb")]
mod caveat_context;
#[cfg(feature = "spicedb")]
mod caveat_context_error;
#[cfg(feature = "spicedb")]
mod check_outcome;
#[cfg(feature = "spicedb")]
mod check_request;
mod civil;
mod claim;
mod claim_lookup_error;
mod claim_property_key;
mod claim_set;
mod claim_text;
mod claim_type;
mod claim_value;
mod client_id;
mod confirmation;
pub mod constants;
#[cfg(feature = "axum")]
mod credential_outcome;
mod current_actor;
#[cfg(feature = "spicedb")]
mod current_actor_subject;
mod current_principal;
mod delegation;
mod dynamic_claims;
mod errors;
mod group_name;
mod identity;
mod identity_builder;
#[cfg(feature = "json")]
pub mod ingest;
#[cfg(feature = "json")]
mod ingest_error;
mod ingest_profile;
mod ingested;
mod issuer;
mod jwk;
mod kind;
#[cfg(feature = "macros")]
mod macros;
#[cfg(feature = "axum")]
mod masked;
mod name_claim_type;
mod numeric_date;
#[cfg(feature = "spicedb")]
mod object_id;
#[cfg(feature = "spicedb")]
mod object_reference;
#[cfg(feature = "spicedb")]
mod object_type;
#[cfg(feature = "spicedb")]
mod outcome_error;
#[cfg(feature = "spicedb")]
mod param_name;
#[cfg(feature = "axum")]
mod presented_proof;
mod principal;
mod principal_builder;
mod principal_key;
mod principal_kind;
#[cfg(feature = "axum")]
mod principal_unavailable;
mod prior_actor;
#[cfg(feature = "json")]
mod raw_claims;
#[cfg(feature = "json")]
mod raw_claims_error;
#[cfg(feature = "json")]
mod raw_json;
#[cfg(feature = "json")]
mod raw_scan;
#[cfg(feature = "spicedb")]
mod reference_parse_error;
mod refinement;
#[cfg(feature = "axum")]
mod refinement_extract;
#[cfg(feature = "spicedb")]
mod relation_name;
#[cfg(feature = "axum")]
mod request_extensions;
mod role_claim_type;
mod role_name;
mod scope;
mod sealed;
mod standard_claims;
#[cfg(feature = "axum")]
mod step_up;
mod subject_id;
#[cfg(feature = "spicedb")]
mod subject_mapping;
#[cfg(feature = "spicedb")]
mod subject_reference;
#[cfg(feature = "spicedb")]
mod subject_set;
mod timestamp;
#[cfg(feature = "axum")]
mod unauthenticated;
mod unsigned63;
#[cfg(feature = "spicedb")]
mod unverified_directed_principal;
mod utc_offset;
mod value_object;
mod verified_binding;
#[cfg(feature = "axum")]
mod web_extractors;
mod well_known_claim;

#[doc(hidden)]
#[cfg(feature = "macros")]
pub mod __private;

pub use actor_error::ActorError;
pub use actor_issuer::ActorIssuer;
pub use actor_key::ActorKey;
#[cfg(feature = "spicedb")]
pub use actor_subject_error::ActorSubjectError;
pub use audience::Audience;
pub use authenticated_principal::{AuthenticatedPrincipal, NotAuthenticatedError, RefinementError};
pub use authentication_context::AuthenticationContext;
pub use authentication_method::AuthenticationMethod;
pub use authentication_type::AuthenticationType;
#[cfg(feature = "spicedb")]
pub use authorization_subject::AuthorizationSubject;
pub use authorized_actor::AuthorizedActor;
pub use binding::Binding;
pub use bootstrap_credential::BootstrapCredential;
#[cfg(feature = "spicedb")]
pub use caveat_context::{
    CaveatContext, CaveatContextBuilder, CheckContext, CheckTime, ContextPhase, ParamDeclaration, WriteContext,
    WriteTime,
};
#[cfg(feature = "spicedb")]
pub use caveat_context_error::CaveatContextError;
#[cfg(feature = "spicedb")]
pub use check_outcome::{CheckOutcome, LookupOutcome};
#[cfg(feature = "spicedb")]
pub use check_request::CheckRequest;
pub use claim::{Claim, ClaimBuilder};
pub use claim_lookup_error::ClaimLookupError;
pub use claim_property_key::ClaimPropertyKey;
pub use claim_set::{ClaimSet, HasRoles};
#[cfg(feature = "spicedb")]
pub use claim_set::{HasSubject, SubjectRef};
pub use claim_text::ClaimText;
pub use claim_type::ClaimType;
pub use claim_value::ClaimValue;
pub use client_id::ClientId;
pub use confirmation::Confirmation;
pub use current_actor::{CurrentActor, PriorActors};
pub use current_principal::CurrentPrincipal;
pub use delegation::Delegation;
pub use dynamic_claims::DynamicClaims;
pub use errors::{NumericDateError, TimestampError, Unsigned63Error, UtcOffsetError, ValueKind, ValueObjectError};
pub use group_name::{GroupName, GroupNameError};
pub use identity::{AnonymousIdentityError, AuthenticatedIdentity, Identity};
pub use identity_builder::{IdentityBuildError, IdentityBuilder};
#[cfg(feature = "json")]
pub use ingest_error::{ClaimValueError, ConfirmationError, IngestError};
pub use ingest_profile::IngestProfile;
pub use ingested::Ingested;
pub use issuer::Issuer;
pub use jwk::Jwk;
pub use kind::{Classified, Kind, Unclassified};
pub use name_claim_type::NameClaimType;
pub use numeric_date::NumericDate;
#[cfg(feature = "spicedb")]
pub use object_id::ObjectId;
#[cfg(feature = "spicedb")]
pub use object_reference::ObjectReference;
#[cfg(feature = "spicedb")]
pub use object_type::ObjectType;
#[cfg(feature = "spicedb")]
pub use outcome_error::OutcomeError;
#[cfg(feature = "spicedb")]
pub use param_name::ParamName;
pub use principal::{Principal, SourcedClaim};
pub use principal_builder::{PrimaryIdentityIndex, PrincipalBuildError, PrincipalBuilder};
pub use principal_key::PrincipalKey;
pub use principal_kind::PrincipalKind;
pub use prior_actor::PriorActor;
#[cfg(feature = "json")]
pub use raw_claims::RawClaims;
#[cfg(feature = "json")]
pub use raw_claims_error::RawClaimsError;
#[cfg(feature = "json")]
pub use raw_json::{RawJson, RawKind};
#[cfg(feature = "spicedb")]
pub use reference_parse_error::ReferenceParseError;
pub use refinement::{
    AnyBinding, AnyDelegation, BindingState, BoundProof, Delegated, DelegationState, Direct, KindState, KnownKind,
    Refined, Refinement, SenderConstrained, WithBinding, WithDelegation, WithKind,
};
#[cfg(feature = "spicedb")]
pub use relation_name::RelationName;
pub use role_claim_type::RoleClaimType;
pub use role_name::RoleName;
pub use scope::{ScopeName, ScopeNameError, ScopeSet};
pub use standard_claims::{ParsedStandardClaims, StandardClaims};
pub use subject_id::SubjectId;
#[cfg(feature = "spicedb")]
pub use subject_mapping::{Base64UrlUnpadded, SubjectMapping, SubjectMappingError, Verbatim};
#[cfg(feature = "spicedb")]
pub use subject_reference::SubjectReference;
#[cfg(feature = "spicedb")]
pub use subject_set::SubjectSet;
pub use timestamp::Timestamp;
pub use unsigned63::Unsigned63;
#[cfg(feature = "spicedb")]
pub use unverified_directed_principal::UnverifiedDirectedPrincipal;
pub use utc_offset::UtcOffset;
pub use verified_binding::{BindingError, BindingMethod, Dpop, Method, MutualTls, VerifiedBinding};
pub use well_known_claim::WellKnownClaim;
