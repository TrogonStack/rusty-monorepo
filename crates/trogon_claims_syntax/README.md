# trogon_claims_syntax

**Validators for the identifier shapes that `trogon_claims` accepts, with no
dependencies and no allocation.** The same rules run at compile time inside a
procedural macro and at run time inside a constructor, so a literal and a parsed
string can never disagree about what is valid.

The crate is `no_std`. Each `validate_*` function is a `const fn` that returns a
violation naming the rule, the byte offset and the offending byte, or `Ok(())`.

## Reference

- Object ids, object types, relation names and caveat parameter names follow
  SpiceDB schema syntax.
- Principal kinds, subject ids and client ids follow the rules the claims model
  applies at ingest.
- The full list is in the [crate documentation](src/lib.rs).

## Use

Most readers want [`trogon_claims`](../trogon_claims/README.md), which wraps these
validators in value objects. Depend on this crate directly only to write another
macro over the same rules.

```rust
use trogon_claims_syntax::validate_object_id;

assert!(validate_object_id("user-1").is_ok());
assert!(validate_object_id("*").is_err());
```

## Consuming

The crate is not published to crates.io. Depend on it by git revision, pinned to a
commit, as described in the [`trogon_claims` README](../trogon_claims/README.md#consuming-the-crates).
