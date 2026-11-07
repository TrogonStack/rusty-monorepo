# trogon_claims

Raw JSON claims, compact JWT payload parsing, introspection and generic claim projection.

Credential verification and resource authorization remain caller responsibilities.
All optional features are disabled by default.

## Features

- `json`: raw JSON, JWT payloads, introspection and claim projection.

## Consuming the crates

This crate is unpublished. Consume it by a pinned Git revision.

The default feature set is empty. Enable `ws-claims` for the WS claim-name
constants and `WellKnownClaim::Ws*` recognition. Without it, nonempty WS URI
names remain exact custom `ClaimType` values. JWT/OIDC names and explicit
name/role selectors remain available. Claim value datatype metadata is independent.
