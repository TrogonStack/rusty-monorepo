# trogon_object_id

Domain-specific prefixed identifiers for Rust, inspired by
[`trogon_object_id` for Elixir](https://hex.pm/packages/trogon_object_id/0.3.0).
A user ID and an order ID are different Rust types. Parsing validates the entire
prefix and the raw value before constructing either type.

## Quick start

```rust
use trogon_object_id::{object_id, union_object_id};

object_id! { pub struct UserId("user"); }
object_id! { pub struct ServiceId("service"); }
union_object_id! {
    pub enum PrincipalId {
        User(UserId),
        Service(ServiceId),
    }
}

let user = UserId::try_from_raw("abc-123")?;
assert_eq!(user.to_string(), "user_abc-123");
assert_eq!("user_abc-123".parse::<UserId>()?, user);
let principal = PrincipalId::from(user);
match principal {
    PrincipalId::User(user) => assert_eq!(user.as_raw(), "abc-123"),
    PrincipalId::Service(_) => unreachable!(),
}
# Ok::<(), trogon_object_id::ParseError>(())
```

## Validation

Declarations require an ASCII letter followed by ASCII letters, digits, `_`, or
`-`, with an object type of at most 64 bytes. The separator defaults to `_` and
must contain between 1 and 4 ASCII punctuation characters. Invalid declarations
fail during compilation. These restrictions keep prefixes stable across systems.

Raw values retain their exact text, including Unicode. They must contain between
1 and 1024 UTF-8 bytes and contain no Unicode control characters. The bound is
checked before allocating a copy. Parsing requires the complete, case-sensitive
prefix; it never silently accepts raw storage values. Errors describe the failed
constraint without retaining or displaying the supplied ID.

Use a validator to constrain a domain further:

```rust
use trogon_object_id::{object_id, validators::Integer};

object_id! {
    pub struct InvoiceId("invoice", separator = ":", validator = Integer);
}

let id: InvoiceId = "invoice:+00042".parse()?;
assert_eq!(id.as_raw(), "+00042");
# Ok::<(), trogon_object_id::ParseError>(())
```

`Integer` accepts an optional ASCII sign followed by ASCII decimal digits, without
numeric overflow or text normalization. Custom validators implement `Validator`.
Their code is trusted application code: keep validation deterministic, avoid
side effects, and return `ParseError::InvalidValue` for domain failures. The core
checks the length and controls before invoking a custom validator.

## Storage and JSON

Use `Display` and `FromStr` for full prefixed storage. Use `as_raw` and
`try_from_raw` only when the column or field already establishes the domain.
These explicit operations replace Elixir's implicit Ecto format configuration.
A union always stores its complete prefixed form so its discriminator is retained.

The optional `serde` feature serializes and deserializes full strings, using the
same validated parser. For an explicitly raw JSON field, use the
`trogon_object_id::serde_raw` adapter with Serde's `with` field attribute. It also
validates on deserialization. It cannot be used with a union.

## UUID generation

Enable the `uuid` feature for `validators::Uuid` and the `Uuid4` and `Uuid7`
generators. UUID values must use hyphenated text; accepted case is preserved.

```rust
# #[cfg(feature = "uuid")]
# {
use trogon_object_id::{object_id, validators::Uuid, Uuid4, Uuid7};

object_id! { pub struct SessionId("session", validator = Uuid); }
let random = SessionId::generate::<Uuid4>()?;
let ordered = SessionId::generate::<Uuid7>()?;
# }
# Ok::<(), trogon_object_id::ParseError>(())
```

Generation uses the `uuid` crate and its system randomness. Generator and
validator types must match at compile time. IDs are identifiers, not credentials;
UUID version 7 exposes a timestamp and neither format establishes authorization.

## Unions and compatibility

`union_object_id!` creates an exhaustive enum and `From` conversions from its
member types. Definitions with duplicate or overlapping complete prefixes fail
at compile time, even when the enum is unused. For example, `app_` and
`app_admin_` overlap, while `app_` and `apple_` do not. This deliberately avoids
Elixir 0.3.0's documented first-match ambiguity. A known prefix with an invalid
value returns that member's validation failure.

This crate preserves the upstream prefixed string format, exact raw text,
configurable separators, validation, generation, and typed unions. It strengthens
raw-value bounds, control-character handling, declaration validation, and union
collision checks. Ecto, Phoenix, and protobuf annotation integration are specific
to the Elixir environment and are not included. Rust traits, explicit storage
operations, and optional Serde provide the corresponding native boundaries.

## Cargo features

The default build has no optional features. `serde` enables Serde and raw field
adapters. `uuid` enables UUID validation and generation. The implementation
forbids unsafe Rust and does not introduce a runtime registry.
