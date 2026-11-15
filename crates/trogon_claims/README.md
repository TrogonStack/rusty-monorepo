# trogon_claims

Validated scalar claim values and protocol syntax.

Credential verification and resource authorization remain caller responsibilities.
All optional features are disabled by default.

## Consuming the crates

This crate is unpublished. Consume it by a pinned Git revision.

The default feature set is empty. Enable `ws-claims` for the WS claim-name
constants. Claim value datatype metadata remains available independently.
