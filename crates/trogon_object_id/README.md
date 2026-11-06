# trogon_object_id

Domain-specific prefixed identifiers for Rust, inspired by
[`trogon_object_id` for Elixir](https://hex.pm/packages/trogon_object_id/0.3.0).
`ObjectId<UserDomain>` and `ObjectId<OrderDomain>` are distinct types. A domain
chooses its prefix and its payload representation through ordinary Rust traits.
Macros are optional declaration conveniences.

## Quick start

Enable the `uuid` feature for compact UUID identifiers:

```rust
# #[cfg(feature = "uuid")]
# {
use trogon_object_id::{Definition, Domain, ObjectId, Uuid, Uuid7};

pub struct UserDomain;
impl Domain for UserDomain {
    const DEFINITION: Definition = Definition::new("user", "_");
    type Payload = Uuid;
}

pub type UserId = ObjectId<UserDomain>;

let user = UserId::generate::<Uuid7>()?;
let encoded = user.to_string();
let restored: UserId = encoded.parse()?;
assert_eq!(restored, user);
let copied = user;
assert_eq!(copied, user);
assert_eq!(std::mem::size_of::<UserId>(), 16);
# }
# Ok::<(), trogon_object_id::ParseError>(())
```

The marker needs no derives. `Clone`, `Copy`, `Debug`, equality, ordering, and
hashing depend only on the payload's traits. UUID IDs store a `uuid::Uuid`
directly and are `Copy`; their domain marker occupies no storage.

For spelling-preserving text, choose `Text` or `Text<Validator>`:

```rust
use trogon_object_id::{validators::Integer, Definition, Domain, ObjectId, Text};

struct InvoiceDomain;
impl Domain for InvoiceDomain {
    const DEFINITION: Definition = Definition::new("invoice", ":");
    type Payload = Text<Integer>;
}
type InvoiceId = ObjectId<InvoiceDomain>;

let raw = String::from("+00042");
let original_buffer = raw.as_ptr();
let invoice = InvoiceId::try_from_owned(raw)?;
assert_eq!(invoice.as_raw(), "+00042");
assert_eq!(invoice.as_str().as_ptr(), original_buffer);
assert_eq!(invoice.to_string(), "invoice:+00042");
# Ok::<(), trogon_object_id::ParseError>(())
```

## Input and storage boundaries

`FromStr` and `TryFrom<&str>` accept only the complete prefixed form.
`TryFrom<String>` also accepts the complete form; for text it removes the prefix
in place and reuses the existing allocation. `try_from_raw` and `try_from_owned`
accept an unprefixed suffix when the field or column already establishes its
domain. A wrong prefix is never silently treated as a raw value.

`payload` borrows the typed payload and `into_payload` returns ownership.
`try_from_payload` validates an existing typed payload before wrapping it.
`as_raw` borrows the raw representation: `&str` for text or `&Uuid` for a compact
UUID. Both support `Display` without allocating a temporary string.

Raw values must contain between 1 and 1024 UTF-8 bytes and no Unicode control
characters. Text keeps its exact spelling, including Unicode and integer signs
or leading zeros. `Integer` validates signed ASCII decimal syntax without a
numeric range. Custom text rules implement `Validator` and use `Text<V>`;
validation must be deterministic and free of side effects.

`Payload` is the extension point for other typed representations. Its
implementation is trusted to enforce immutable, valid state and the documented
text bounds on all parsing paths. Prefer `Text<V>` when only a text rule changes.
The built-in parsers reject malformed input before allocating owned storage.
Errors describe the failed constraint without retaining or displaying the input.

## Compile-time declarations

`Definition::new` is a `const` constructor. It requires an ASCII letter followed
by ASCII letters, digits, `_`, or `-`, with at most 64 bytes in the object type.
Separators contain between 1 and 4 ASCII punctuation characters. An invalid
associated definition fails when it is evaluated; to check an otherwise unused
handwritten domain eagerly, declare `const _: Definition = DomainName::DEFINITION`.
The declaration macro emits this assertion automatically.

```rust
use trogon_object_id::{object_id, validators::Integer};

object_id! { pub struct UserId("user"); }
object_id! { pub struct InvoiceId("invoice", separator = ":", validator = Integer); }
let user: UserId = "user_abc-123".parse()?;
assert_eq!(user.as_raw(), "abc-123");
# Ok::<(), trogon_object_id::ParseError>(())
```

The optional macro creates a distinct wrapper around `ObjectId`.
`validator = V` selects `Text<V>`. `payload = Uuid` selects compact UUID storage.
Use handwritten domain markers when you want the generic ID's conditional
`Copy` implementation without adding a wrapper.

## Unions

An ordinary Rust enum can hold identifiers from different domains.
`union_object_id!` adds prefixed parsing, `Display`, member conversions and
optional Serde implementations. It accepts aliases of handwritten generic IDs:

```rust
use trogon_object_id::{union_object_id, Definition, Domain, ObjectId, Text};

struct UserDomain;
impl Domain for UserDomain {
    const DEFINITION: Definition = Definition::new("user", "_");
    type Payload = Text;
}
struct ServiceDomain;
impl Domain for ServiceDomain {
    const DEFINITION: Definition = Definition::new("service", "_");
    type Payload = Text;
}
type UserId = ObjectId<UserDomain>;
type ServiceId = ObjectId<ServiceDomain>;

union_object_id! { enum PrincipalId { User(UserId), Service(ServiceId) } }
let principal: PrincipalId = "user_abc-123".parse()?;
assert!(matches!(principal, PrincipalId::User(_)));
# Ok::<(), trogon_object_id::ParseError>(())
```

Union definitions reject duplicate or overlapping complete prefixes during
compilation, even when unused. `app_` and `app_admin_` overlap; `app_` and
`apple_` do not. Handwritten union parsers can call `assert_disjoint` in a
`const` assertion for the same definition check. A union always stores the
complete prefixed form so its discriminator is retained.

## JSON

Enable `serde` to serialize and deserialize generic IDs or macro wrappers as
full strings. Decoding uses the same checked parser. The `serde_raw` field
adapter explicitly drops the prefix for a field whose type establishes its
domain; it validates on deserialization and cannot be used with a union.

Serialization writes through `Serializer::collect_str`. The serializer and its
output destination determine whether output storage allocates. Borrowing
string visitors let UUID decoding avoid intermediate owned text; text decoding
reuses an owned string when the deserializer supplies one.

## UUID generation and representation

The `uuid` feature provides `Uuid`, `Uuid4`, and `Uuid7`. Generic generation
requires a generator with the domain's exact payload type. A generator cannot
silently substitute text for a UUID or bypass payload validation.

Compact UUID payloads accept hyphenated text and display canonical lowercase
hyphenated text. UUID text capitalization is normalized. To preserve exact
spelling instead, choose `Text<validators::Uuid>` and construct from text.
That choice intentionally keeps owned text allocation and cloning costs.

UUID parsing, copying and formatting into an existing buffer need no heap
allocation in this crate. UUID generation uses the `uuid` crate's system
randomness and time behavior; its randomness failure can panic, even though the
shared generation API returns `Result` for fallible custom generators. UUID
version 7 exposes a timestamp. Identifiers do not establish authorization.

## Performance and compatibility

Domain markers and static trait dispatch add no stored domain data, vtables or
runtime registry. `ObjectId` has the same layout as its payload. Text parsed
from `&str` allocates once; owned text is validated and moved. Cloning text
copies its string, while UUID IDs are `Copy`. Validation, entropy acquisition,
formatting work and enum discriminators still have real costs.

This crate preserves the upstream prefixed wire format, configurable separators,
text validation and typed unions. It strengthens raw-value bounds,
control-character handling, declaration validation and union collision checks.
Compact UUIDs canonicalize text. Ecto, Phoenix and protobuf annotations are
specific to the Elixir environment and are not included.

The default build has no optional features. `serde` enables JSON-compatible
traits and raw field adapters. `uuid` enables compact UUID payloads and
generation. The implementation forbids unsafe Rust.
