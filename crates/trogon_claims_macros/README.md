# trogon_claims_macros

**Procedural macros that reject an invalid identifier literal while the compiler
is expanding it.** They back `object_id!`, `object_type!`, `principal_kind!`,
`relation_name!` and `param_name!`, so a typo in a literal is a compile error that
points at the literal instead of a runtime `Result`.

The crate depends only on `trogon_claims_syntax`, so it carries no `syn` or
`quote` in the build.

## Do not depend on this crate directly

The `*_impl` macros take the path of the calling crate as their first argument and
are meant to be reached through the declarative macros that
[`trogon_claims`](../trogon_claims/README.md) exports. Call those instead:

```rust
use trogon_claims::object_id;

let id = object_id!("user-1");
```

## Reference

- Raw string literals are accepted.
- Literals with escape sequences are refused, because no valid value needs one.
- A non-literal argument is rejected by the declarative front end before the
  procedural macro runs.

## Consuming

The crate is not published to crates.io. It is built as a dependency of
`trogon_claims`; see the
[consumption notes](../trogon_claims/README.md#consuming-the-crates).
