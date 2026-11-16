# trogon_claims_syntax

**Validators for the identifier shapes that `trogon_claims` accepts, with no
dependencies and no allocation.** The same rules run at compile time inside a
procedural macro and at run time inside a constructor, so a literal and a parsed
string can never disagree about what is valid.

The crate is `no_std`. Each `validate_*` function is a `const fn` that returns a
violation naming the rule, the byte offset and the offending byte, or `Ok(())`.

## Reference

- Object ids, object types, relation names and caveat parameter names follow
  SpiceDB schema syntax behind the default-off `spicedb` feature.
- Principal kinds, subject ids and client ids follow the rules the claims model
  applies at ingest.
- The full list is in the [crate documentation](src/lib.rs).

`spicedb` is forwarded by `trogon_claims/spicedb`. Core claims identifiers do
not use the backend grammar.

The object-id rule follows [Authzed `ObjectReference.object_id`](https://github.com/authzed/api/blob/3270b08244f3e97e2a57a7ee2d9c649c88b861d3/authzed/api/v1/core.proto#L106-L115):
a nonempty `[a-zA-Z0-9/_|\-=+]` value of at most 1024 bytes, or a standalone
wildcard. This crate deliberately rejects the wildcard for concrete `ObjectId`
values; that restriction is local policy, not an upstream grammar requirement.

## Use

Most readers want [`trogon_claims`](../trogon_claims/README.md), which wraps these
validators in value objects. Depend on this crate directly only to write another
macro over the same rules.

With `spicedb` enabled:

```rust
use trogon_claims_syntax::validate_object_id;

assert!(validate_object_id("user-1").is_ok());
assert!(validate_object_id("*").is_err());
```

## Consuming

The crate is not published to crates.io. Depend on it by git revision, pinned to a
commit, as described in the [`trogon_claims` README](../trogon_claims/README.md#consuming-the-crates).
