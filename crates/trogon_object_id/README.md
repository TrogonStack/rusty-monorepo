# trogon_object_id

Domain-specific prefixed identifiers for Rust, inspired by
[`trogon_object_id` for Elixir](https://hex.pm/packages/trogon_object_id/0.3.0).
`ObjectId<UserDomain>` and `ObjectId<OrderDomain>` are distinct types. Declare
ordinary Rust domain structs and union enums, then derive their supporting
implementations. The typed core also supports handwritten trait implementations.

## Quick start

Enable the `derive` and `uuid` features for derived domains with compact UUID
payloads:

```rust
# #[cfg(all(feature = "derive", feature = "uuid"))]
# {
use trogon_object_id::{ObjectId, ObjectIdDomain, Uuid, Uuid7};

#[derive(ObjectIdDomain)]
#[object_id(prefix = "user", payload = Uuid)]
pub struct UserDomain;

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

`ObjectIdDomain` implements `Domain` for the unit struct. Its helper attribute
`#[object_id(...)]` supplies the prefix, separator, and payload type. `prefix`
excludes the separator, which defaults to `_`; the payload defaults to `Text`.
The domain struct remains a zero-sized marker. `Clone`, `Copy`, `Debug`, equality,
ordering, and hashing on an ID depend only on its payload's traits. UUID IDs
store a `uuid::Uuid` directly and are `Copy`.

For spelling-preserving text, choose `Text` or `Text<Validator>`:

```rust
# #[cfg(feature = "derive")]
# {
use trogon_object_id::{validators::Integer, ObjectId, ObjectIdDomain, Text};

#[derive(ObjectIdDomain)]
#[object_id(prefix = "invoice", separator = ":", payload = Text<Integer>)]
struct InvoiceDomain;

type InvoiceId = ObjectId<InvoiceDomain>;

let raw = String::from("+00042");
let original_buffer = raw.as_ptr();
let invoice = InvoiceId::try_from_owned(raw)?;
assert_eq!(invoice.as_raw(), "+00042");
assert_eq!(invoice.as_str().as_ptr(), original_buffer);
assert_eq!(invoice.to_string(), "invoice:+00042");
# }
# Ok::<(), trogon_object_id::ParseError>(())
```

## Unions

Declare the variants as an ordinary Rust enum. `ObjectIdUnion` adds prefixed
parsing, `Display`, member conversions, and Serde implementations when the
`serde` feature is enabled. Standard derives such as `Debug` and `Clone` remain
explicit on the enum:

```rust
# #[cfg(feature = "derive")]
# {
use trogon_object_id::{ObjectId, ObjectIdDomain, ObjectIdUnion};

#[derive(ObjectIdDomain)]
#[object_id(prefix = "user")]
struct UserDomain;

#[derive(ObjectIdDomain)]
#[object_id(prefix = "service")]
struct ServiceDomain;

type UserId = ObjectId<UserDomain>;
type ServiceId = ObjectId<ServiceDomain>;

#[derive(Debug, Clone, PartialEq, Eq, ObjectIdUnion)]
enum PrincipalId {
    User(UserId),
    Service(ServiceId),
}

let principal: PrincipalId = "user_abc-123".parse()?;
assert!(matches!(principal, PrincipalId::User(_)));
# }
# Ok::<(), trogon_object_id::ParseError>(())
```

Every union variant holds exactly one identifier. The derive accepts aliases of
IDs from derived or handwritten domains. Normal `#[cfg(...)]` and
`#[cfg_attr(...)]` attributes control which declarations and variants exist.
A union always stores the complete prefixed form so its discriminator is
retained.

## Compile-time declarations

Domain definitions require an ASCII letter followed by ASCII letters, digits,
`_`, or `-`, with at most 64 bytes in the prefix. Separators contain between 1
and 4 ASCII punctuation characters. `ObjectIdDomain` checks these constraints
during compilation, even when the domain is unused.

`ObjectIdUnion` rejects duplicate or overlapping complete prefixes during
compilation, even when the enum is unused. `app_` and `app_admin_` overlap;
`app_` and `apple_` do not. Unknown attributes, unsupported declaration shapes,
and mismatched domain types fail to compile.

The compiler also keeps domains distinct when their payload types are equal:

```compile_fail
use trogon_object_id::{Definition, Domain, ObjectId, Text};

struct UserDomain;
impl Domain for UserDomain {
    const DEFINITION: Definition = Definition::new("user", "_");
    type Payload = Text;
}
struct OrderDomain;
impl Domain for OrderDomain {
    const DEFINITION: Definition = Definition::new("order", "_");
    type Payload = Text;
}

fn find_user(_: ObjectId<UserDomain>) {}
let order = ObjectId::<OrderDomain>::try_from_raw("123").unwrap();
find_user(order);
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

## JSON

Enable `serde` to serialize and deserialize generic IDs and derived unions as
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

## Using the core without derives

The `derive` feature is optional. An ordinary `Domain` implementation provides
the same ID behavior without a procedural macro:

```rust
use trogon_object_id::{Definition, Domain, ObjectId, Text};

struct ExternalDomain;
impl Domain for ExternalDomain {
    const DEFINITION: Definition = Definition::new("external", ":");
    type Payload = Text;
}

type ExternalId = ObjectId<ExternalDomain>;
let external: ExternalId = "external:abc-123".parse()?;
assert_eq!(external.as_raw(), "abc-123");
# Ok::<(), trogon_object_id::ParseError>(())
```

`Definition::new` is a `const` constructor. An invalid associated definition
fails when evaluated; to check an otherwise unused handwritten domain eagerly,
declare `const _: Definition = DomainName::DEFINITION`. Handwritten union parsers
can call `assert_disjoint` in a `const` assertion for the same prefix checks.

## Performance and compatibility

Domain markers and static trait dispatch add no stored domain data, vtables or
runtime registry. `ObjectId` has the same layout as its payload. Text parsed
from `&str` allocates once; owned text is validated and moved. Cloning text
copies its string, while UUID IDs are `Copy`. Derives generate implementations
during compilation and introduce no runtime registry. Validation, entropy
acquisition, formatting work and enum discriminators still have real costs.

This crate preserves the upstream prefixed wire format, configurable separators,
text validation and typed unions. It strengthens raw-value bounds,
control-character handling, declaration validation and union collision checks.
Compact UUIDs canonicalize text. Ecto, Phoenix and protobuf annotations are
specific to the Elixir environment and are not included.

The default build has no optional features. `derive` enables `ObjectIdDomain`
and `ObjectIdUnion`. `serde` enables JSON-compatible traits and raw field
adapters. `uuid` enables compact UUID payloads and generation. The implementation
forbids unsafe Rust.

## License

Licensed under [Apache-2.0](LICENSE).
