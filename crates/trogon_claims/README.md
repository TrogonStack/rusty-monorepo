# trogon_claims

**Claims-based identity with explicit policy and typed boundaries.** Represent
claims, identities and principals as immutable Rust values. Select name and role
claims deliberately, retain identity provenance when querying a principal, and
optionally project authorization subjects through a validated SpiceDB mapping.

**It starts where token verification ends.** The calling authentication layer
verifies credentials and token acceptance policy before constructing an
authenticated identity. Construction validates the identity representation; it
does not verify a signature or grant resource access. `AuthenticatedPrincipal`
proves that the supplied principal has an authenticated primary identity, not
that this crate performed credential verification.

**It is for services that authenticate with one library and authorize with
another.** The seam between the two is where identifiers get stringly typed and
where "empty name means anonymous" bugs live. The crate makes that seam a set of
types.

## Documentation

### Tutorial

- Build a principal in the [crate documentation](src/lib.rs).

### How-to

- [Consume the crates](#consuming-the-crates)
- [Turn on a feature](#features)
- [Interpret standard claims](#typed-standard-claims)
- [Compose identities and query their provenance](#principal-composition-and-queries)

### Reference

- The .NET to Rust mapping and every departure from .NET semantics are in the
  [crate documentation](src/lib.rs).
- Feature and dependency budget: [Features](#features).
- HTTP behaviour of the `axum` feature: [Challenges](#challenges).
- Claim specifications are linked beside the variants in
  [`WellKnownClaim`](src/well_known_claim.rs); delegation, binding and HTTP types
  link to their defining RFC sections in their API documentation.

### Explanation

- Why anonymous is a variant, why roles are not decisions, and why the
  authenticated wrapper has no `Deref`: see "Where this departs from .NET" in the
  [crate documentation](src/lib.rs).
- [Claims model and protocol semantics](#claims-model-and-protocol-semantics).

## Claims model and protocol semantics

.NET is a reference for the principal/identity/claim organization. The wire meanings come
from JWT, OpenID Connect and the OAuth RFCs cited beside the relevant types.
Those citations describe the source of a claim or challenge, not a claim that
this crate implements a complete token-validation profile. Signature, issuer,
audience and token-lifetime validation belong to the verifying layer.

`IdentityBuilder` and `IngestProfile` require a `NameClaimType` and a
`RoleClaimType` at construction. These distinct types make both choices
explicit and prevent swapping their positions. JWT/OIDC claim names are kept
as received; ingest does not map `name` or `roles` to WS-* URIs.

For example, a service can choose `NameClaimType::new(WellKnownClaim::Sub.into())`
and `RoleClaimType::new(WellKnownClaim::Roles.into())`. A service using the .NET
vocabulary can explicitly choose `WsName` and `WsRole`. Each selector chooses
one claim type: selecting `PreferredUsername` does not fall back to `Sub`, and
selecting `Roles` does not also use `Groups`. A missing or non-text selected name
returns `None`. Selected role assertions can be repeated non-empty text or one
flat list of non-empty text. Malformed or partially projected collections never
match a role query.
Neither query falls back to another claim type. The authentication
key remains issuer plus subject, independently of the selected principal name.

[MicroProfile JWT 2.1](https://download.eclipse.org/microprofile/microprofile-jwt-auth-2.1/microprofile-jwt-auth-spec-2.1.html)
is a useful JVM comparison: `JsonWebToken` extends `Principal`, its `Claims` enum
names JWT/OIDC claims, principal names resolve through `upn`, then
`preferred_username`, then `sub`, and `groups` supplies caller roles. This crate
does not implement that fallback or adopt the MicroProfile validation profile.
Its delegation and sender-constraint types are instead grounded in RFC 8693 and
the confirmation and proof-of-possession RFCs.

[Spring Security's authentication model](https://docs.spring.io/spring-security/reference/servlet/authentication/architecture.html)
also separates a principal from its granted authorities. Here, verified claims
remain assertions to query, and a resource permission check remains a separate
decision. Authentication refinements are immutable Rust types passed explicitly
to the caller, rather than ambient security-context state.

## Identity construction and trust

An authentication key is the issuer plus subject. A principal name is a selected
claim for display or application lookup; changing that selection does not change
the key. With `spicedb`, authorization subject mapping is another explicit
operation and can reject a subject that the identity model can represent.
Without that feature, identities and typed claim sets retain their original
`SubjectId` through `PrincipalKey`; they do not require a backend object type
or object ID.

Identity construction requires a textual subject and issuer. A caller may supply
an explicit default issuer when the input omits it. An explicitly malformed issuer
must fail rather than use that default. Claim attribution (`Claim::issuer`) records
who asserted a claim; it does not silently substitute for the identity's issuer.
For locally constructed claims, omitting `ClaimBuilder::issuer` labels the claim
with the local-authority metadata issuer. That attribution convention cannot
satisfy the identity's required issuer field.

The programmatic builder derives the identity key from claims and takes actor,
binding, delegation eligibility and authentication context through its explicit
typed setters. Pushing claims named `act`, `cnf`, `may_act` or `acr` does not
implicitly populate those separate fields. JSON ingest parses those structures
at its boundary.

Additional claims in current or prior `act` actors and `may_act` support scalars
and flat arrays of scalars. Objects, nulls and nested arrays that would lose
claim data cause `UnrepresentableActorClaims` on the enclosing `act` or `may_act`;
ingest never returns a partially projected actor. Nested `act` chains are parsed
separately from these additional claims.

Classification uses `Identity::try_with_kind` and rejects anonymous identities.
Recording binding evidence uses `try_with_verified_binding` and checks that its
method and thumbprint match the identity's existing assertion. Neither operation
performs external credential or proof verification.

These identity-model requirements are distinct from a token profile's required
claims. For example, MicroProfile JWT 2.1 requires `iss`, `iat`, `exp` and a
principal name from `upn`, `preferred_username` or `sub`; it lists `sub`
independently as recommended. This crate requires a subject for its identity key
but does not implicitly adopt the MicroProfile token-validation profile.

The caller owns signature, token kind, trusted-issuer, audience, lifetime and
profile acceptance checks. Parsing `RawClaims` only establishes structural JSON
validity. Authentication context (`acr`, `amr`, `auth_time`), role/group/scope
claims and `may_act` eligibility remain assertions whose acceptability belongs
to application policy. A resource permission result is a separate type.

`ingest::jwt_payload` additionally parses the JWT representation of `iss`, `sub`
and `aud` according to [RFC 7519 §2 and §4.1](https://www.rfc-editor.org/rfc/rfc7519.html#section-2).
For these `StringOrURI` values, a string containing `:` must be a URI under
[RFC 3986](https://www.rfc-editor.org/rfc/rfc3986.html). The parser preserves
the original spelling and case. This grammar belongs to JWT ingestion;
generic `Issuer`, `SubjectId` and `Audience` values remain nonempty opaque text.
Actor identity members use the same representation rules, with bounded nested
`act` chains as described by [RFC 8693 §4](https://www.rfc-editor.org/rfc/rfc8693.html#section-4).
Actor-local `aud`, `exp` and `nbf` do not determine the containing token's validity.

Raw credentials travel separately as `BootstrapCredential`, which has a named
exposure method, redacted formatting and zeroization on drop. They are not a
property of `Principal` or `Identity`. Request identity is passed explicitly or
attached to request extensions; there is no ambient global identity fallback.

## Typed standard claims

Call `identity.claims().parse_standard()` to parse the supported standard claims
into an immutable `ParsedStandardClaims` snapshot. Parsing reports unsupported
shapes, invalid values and ambiguous singleton claims through `ClaimLookupError`.
After parsing succeeds, getters return the stored typed values without parsing
strings again. An absent claim remains `None`; a present empty collection remains
`Some` with an empty slice.

The snapshot exposes issuer, subject, audience, expiration, not-before, issuance
and authentication times, client ID, scopes, roles, groups and common user claims.
Its guarantees concern representation. Select required fields and acceptance
policy in the calling application.

`identity.claims().standard()` remains available for selective interpretation of
individual claims. Each lookup distinguishes:

- `Ok(None)`: the claim is absent.
- `Ok(Some(value))`: the claim has a supported valid representation.
- `Err(ClaimLookupError)`: the value has an unsupported shape, is invalid, or is
  ambiguous because a singleton claim has multiple values.

Both paths preserve JSON shape information: an empty audience array remains a
present empty collection; an object, null, or a partly malformed array cannot
silently become missing or a valid partial value. Generic custom-claim queries
remain available through validated `ClaimType` values.
They read the claim collection itself. An explicitly configured issuer
fallback affects `identity.key().issuer()` without synthesizing an `iss` claim
or rewriting the attribution of existing claims.

`ScopeName`, `RoleName` and `GroupName` are distinct types. Interpreting one of
these assertions does not convert it into another vocabulary or grant resource
access. `NumericDate` accessors interpret timestamps; they do not decide whether
a token is fresh or expired. Select required fields and acceptance policy in the
calling application.

`PrincipalKind` is an application classification, represented as nonempty opaque
text. It has no RFC-defined alphabet or length limit. Backend identifiers use
their own types and grammars behind `spicedb`.

Generic `name` and `is_in_role` queries inspect projected textual assertions;
they do not validate the complete JSON shape of a JWT claim. Use the typed
standard-claim view when policy requires a valid standard representation rather
than a match among projected assertions.

## Principal composition and queries

`Principal::new(identity)` selects its sole identity. A `PrincipalBuilder` with
multiple identities requires `.primary(PrimaryIdentityIndex::new(index))`;
missing, out-of-range or anonymous selections fail. Appending with
`Principal::with_identity` preserves the current primary. Deliberate reselection
uses the fallible `try_with_primary` method.

Principal `claims`, `find_first` and `find_all` queries return `SourcedClaim`,
which exposes both `claim()` and its originating `identity()`. Queries traverse
authenticated identities in insertion order. The selected primary supplies the
principal name; role membership queries inspect each authenticated identity's
explicitly selected role claim. These are queries of assertions, not resource
authorization decisions.

## Features

| Feature | Adds | Extra dependencies |
|---|---|---|
| none | Value objects, claims, identities, principals, refinements and runtime validation | `thiserror`, `zeroize`, `trogon_claims_syntax` |
| `macros` | `principal_kind!`; backend literal constructors also require `spicedb` | `trogon_claims_macros` |
| `json` | `ingest`: JWT payload and introspection to identity | `serde_json`, `fluent-uri` |
| `serde` | Reserved for session serialization; no code yet | `serde` |
| `spicedb` | `ObjectId`, `ObjectType`, subject mapping and `HasSubject`/`SubjectRef`, object/subject references, checks and caveat conversion | `prost`, `prost-types` |
| `axum` | `trogon_claims::axum`: extractors, challenges, step-up responses | `axum-core`, `http` |

The default feature set is empty. Enable `macros` explicitly for
`principal_kind!`. `object_id!`, `object_type!`, `relation_name!` and `param_name!`
require both `macros` and `spicedb`. Runtime constructors never require `macros`;
backend types and their validated constructors require `spicedb`.

`HasSubject`, `SubjectRef`, `SubjectMapping`, `Verbatim` and
`Base64UrlUnpadded` describe the optional backend projection. Their related
identity and principal `subject()` accessors are available only with `spicedb`.
The core identity key, claims, roles, delegation and binding remain available
without that feature. Enabling it does not choose a mapping implicitly.

`.config/mise/tasks/claims/budget` fails CI when a feature pulls in a dependency
outside this table.

## Challenges

With the `axum` feature, a middleware you write attaches the principal, and the
extractors turn a refusal into a response:

| Situation | Response |
|---|---|
| No credentials presented | `401` with a bare `Bearer` challenge and no `error` attribute |
| Credential presented and refused | `401` with `Bearer error="invalid_token"` |
| Proof of possession missing on a `DPoP` route | `401` with a bare `DPoP` challenge |
| Proof of possession does not match | `401` with `DPoP error="invalid_token"` |
| Step-up needed | `401` with `insufficient_user_authentication`, `acr_values` and `max_age` |
| Required kind or delegation not satisfied | `403`, no challenge |
| Middleware not installed | `500` |

The crate does not verify tokens or proofs. The verifying layer supplies a
`CredentialOutcome` and a `PresentedProof`, and the crate compares and renders.
Bearer challenges are defined by
[RFC 6750 §3](https://www.rfc-editor.org/rfc/rfc6750.html#section-3), DPoP by
[RFC 9449 §7.1](https://www.rfc-editor.org/rfc/rfc9449.html#section-7.1), and step-up
parameters by [RFC 9470 §3](https://www.rfc-editor.org/rfc/rfc9470.html#section-3).

## Consuming the crates

The crates are not published to crates.io, and none is listed in the release
configuration. Depend on them by git revision and pin the commit:

```toml
[dependencies]
trogon_claims = { git = "https://github.com/TrogonStack/rusty-monorepo", rev = "<commit>", default-features = false, features = ["json"] }
```

`trogon_claims_syntax` comes along as a core path dependency of that revision.
Enable `macros` to activate the optional `trogon_claims_macros` dependency:

```toml
[dependencies]
trogon_claims = { git = "https://github.com/TrogonStack/rusty-monorepo", rev = "<commit>", default-features = false, features = ["macros"] }
```

```rust
use trogon_claims::principal_kind;

let kind = principal_kind!("person");
```

For `object_id!`, `object_type!`, `relation_name!` and `param_name!`, select
`features = ["macros", "spicedb"]`.
Macro availability is an optional capability following
[ADR 7592933658](https://github.com/straw-hat-team/adr/blob/main/src/adrs/7592933658/README.md);
it does not change the validation required by runtime constructors. Moving to a
registry later is a change to the `publish` setting and release configuration,
not to the API.

## Verify

```bash
mise run claims:check
```

This checks dependency budgets, isolated consumer feature boundaries, the feature
matrix, strict Clippy, tests and documentation. `mise run claims:features` runs
the feature matrix and isolated consumer checks on their own, so a workspace build
cannot hide a missing feature gate through feature unification.
