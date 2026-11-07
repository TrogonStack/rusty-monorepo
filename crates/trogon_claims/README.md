# trogon_claims

Optional literal macros with validated runtime and const constructors.

Credential verification and resource authorization remain caller responsibilities.
All optional features are disabled by default.

## Features

- `json`: raw JSON, JWT payloads, introspection and claim projection with identity ingestion.
- `spicedb`: validated backend subjects, references and check requests.
- `macros`: literal constructors; backend literals also require `spicedb`.

## Consuming the crates

This crate is unpublished. Consume it by a pinned Git revision.
