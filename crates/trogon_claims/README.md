# trogon_claims

**Claims, identities and principals as validated value objects, ready for
SpiceDB.** The crate is the .NET `System.Security.Claims` model expressed in Rust,
with every identifier shaped so it can be written to a SpiceDB check without a
lossy step in between.

**It starts where token verification ends.** It never checks a signature, never
authorizes, and has no async runtime. You bring a verified token payload; it
returns an identity whose anonymous case is a variant, whose authentication is a
type, and whose identifiers were validated once at the boundary.

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

### Reference

- The .NET to Rust mapping and every departure from .NET semantics are in the
  [crate documentation](src/lib.rs).
- Feature and dependency budget: [Features](#features).
- HTTP behaviour of the `axum` feature: [Challenges](#challenges).

### Explanation

- Why anonymous is a variant, why roles are not decisions, and why the
  authenticated wrapper has no `Deref`: see "Where this departs from .NET" in the
  [crate documentation](src/lib.rs).

## Features

| Feature | Adds | Extra dependencies |
|---|---|---|
| none | Value objects, claims, identities, principals, refinements, literal macros | `thiserror`, `zeroize` and the sibling crates |
| `json` | `ingest`: JWT payload and introspection to identity | `serde_json` |
| `serde` | Reserved for session serialization; no code yet | `serde` |
| `spicedb` | Object, subject and subject-set references, check requests, caveat context conversion, tri-state outcomes | `prost`, `prost-types` |
| `axum` | `trogon_claims::axum`: extractors, challenges, step-up responses | `axum-core`, `http` |

`scripts/check-trogon-claims.sh` fails CI when a feature pulls in a dependency
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

## Consuming the crates

The crates are not published to crates.io, and none is listed in the release
configuration. Depend on them by git revision and pin the commit:

```toml
[dependencies]
trogon_claims = { git = "https://github.com/TrogonStack/rusty-monorepo", rev = "<commit>", features = ["json"] }
```

`trogon_claims_syntax` and `trogon_claims_macros` come along as path dependencies
of that revision. Moving to a registry later is a change to the `publish` setting
and the release configuration, not to the API.

## Verify

```bash
cargo test -p trogon_claims --all-features
mise run claims:check
```
