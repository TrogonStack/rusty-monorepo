# trogon_object_id

Domain-specific prefixed identifiers for Rust, inspired by
[`trogon_object_id` for Elixir](https://hex.pm/packages/trogon_object_id/0.3.0).
Declare ordinary Rust newtypes and enums, then derive their supporting
implementations. Payloads remain actual Rust types: UUID identifiers store a
`Uuid`, while text identifiers store validated `Text<V>`. The typed core also
supports domain markers and handwritten trait implementations.

## Quick start

Enable the `macros` and `uuid` features for a compact UUID identifier:

```rust
# #[cfg(all(feature = "macros", feature = "uuid"))]
# {
use trogon_object_id::{ObjectId, Uuid, Uuid7};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ObjectId)]
#[object_id(prefix = "user")]
#[repr(transparent)]
pub struct UserId(Uuid);

let user = UserId::generate::<Uuid7>()?;
let encoded = user.to_string();
let restored: UserId = encoded.parse()?;
assert_eq!(restored, user);
assert_eq!(std::mem::size_of::<UserId>(), 16);
# }
# Ok::<(), trogon_object_id::ParseError>(())
```

`ObjectId` adds checked constructors, parsing, formatting, payload access and
optional Serde implementations. The payload field is private, and
`#[repr(transparent)]` makes its layout explicit. Standard value derives such
as `Debug`, `Clone`, `Copy`, equality and hashing remain ordinary Rust derives.
`UserId` is the identifier type itself; no domain marker or type alias is needed.

The helper attribute `#[object_id(...)]` supplies the prefix and optional
separator. `prefix` excludes the separator, which defaults to `_`. The payload
type is inferred from the tuple field, so it is not repeated in the attribute.

For spelling-preserving text, use `Text` or `Text<Validator>`:

```rust
# #[cfg(feature = "macros")]
# {
use trogon_object_id::{validators::Integer, ObjectId, Text};

#[derive(ObjectId)]
#[object_id(prefix = "invoice", separator = ":")]
#[repr(transparent)]
struct InvoiceId(Text<Integer>);

let raw = String::from("+00042");
let original_buffer = raw.as_ptr();
let invoice = InvoiceId::try_from_owned(raw)?;
assert_eq!(invoice.as_raw(), "+00042");
assert_eq!(invoice.as_raw().as_ptr(), original_buffer);
assert_eq!(invoice.to_string(), "invoice:+00042");
# }
# Ok::<(), trogon_object_id::ParseError>(())
```

## Enums of identifiers

Declare the variants as an ordinary Rust enum. The same `ObjectId` derive adds
prefixed parsing, `Display`, member conversions, and Serde implementations when the
`serde` feature is enabled. Standard derives such as `Debug` and `Clone` remain
explicit on the enum:

```rust
# #[cfg(feature = "macros")]
# {
use trogon_object_id::{ObjectId, Text};

#[derive(Debug, Clone, PartialEq, Eq, ObjectId)]
#[object_id(prefix = "user")]
#[repr(transparent)]
struct UserId(Text);

#[derive(Debug, Clone, PartialEq, Eq, ObjectId)]
#[object_id(prefix = "service")]
#[repr(transparent)]
struct ServiceId(Text);

#[derive(Debug, Clone, PartialEq, Eq, ObjectId)]
enum PrincipalId {
    User(UserId),
    Service(ServiceId),
}

let principal: PrincipalId = "user_abc-123".parse()?;
assert!(matches!(principal, PrincipalId::User(_)));
# }
# Ok::<(), trogon_object_id::ParseError>(())
```

An enum gets its prefixes and payload types from its member IDs; it does not
take `#[object_id(...)]` options. Every variant holds exactly one identifier.
The derive accepts aliases of IDs from derived or handwritten domains. Normal `#[cfg(...)]` and
`#[cfg_attr(...)]` attributes control which declarations and variants exist.
An enum is serialized with the complete prefixed form so its discriminator is
retained.

## Compile-time declarations

Domain definitions require an ASCII letter followed by ASCII letters, digits,
`_`, or `-`, with at most 64 bytes in the prefix. Separators contain between 1
and 4 ASCII punctuation characters. Both identifier derives check these constraints
during compilation, even when the domain is unused.

`ObjectId` on an enum rejects duplicate or overlapping complete prefixes during
compilation, even when the enum is unused. `app_` and `app_admin_` overlap;
`app_` and `apple_` do not. Unknown attributes, unsupported declaration shapes,
and mismatched domain types fail to compile.

The compiler also keeps domains distinct when their payload types are equal:

```compile_fail
use trogon_object_id::{ObjectId, Text};

#[derive(ObjectId)]
#[object_id(prefix = "user")]
#[repr(transparent)]
struct UserId(Text);

#[derive(ObjectId)]
#[object_id(prefix = "order")]
#[repr(transparent)]
struct OrderId(Text);

fn find_user(_: UserId) {}
let order = OrderId::try_from_raw("123").unwrap();
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

Enable `serde` to serialize and deserialize direct IDs, generic IDs and derived
enums as full strings. Decoding uses the same checked parser. The `serde_raw` field
adapter explicitly drops the prefix for a field whose type establishes its
domain; it validates on deserialization and cannot be used with an enum.

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

## Binding a compiled UUIDv5 recipe

Enable `macros` and `template` to select a recipe using a Rust type path:

```rust
# #[cfg(all(feature = "macros", feature = "template"))]
# {
# mod generated {
#     use trogon_object_id::{Uuid, UuidTemplate};
#     pub struct ItemIdentityV1;
#     pub struct ItemIdentityV1Input<'a> { pub external_key: &'a str }
#     impl UuidTemplate for ItemIdentityV1 {
#         type Input<'a> = ItemIdentityV1Input<'a>;
#         const NAMESPACE: Uuid = Uuid::NAMESPACE_DNS;
#         fn uuid5(input: Self::Input<'_>) -> Uuid {
#             Uuid::new_v5(&Self::NAMESPACE, input.external_key.as_bytes())
#         }
#     }
# }
use trogon_object_id::{ObjectId, Uuid};

#[derive(ObjectId)]
#[object_id(prefix = "item", template = generated::ItemIdentityV1)]
#[repr(transparent)]
struct ItemId(Uuid);

let item = ItemId::from_identity(generated::ItemIdentityV1Input {
    external_key: "example.org",
})?;
assert_eq!(item.payload().get_version_num(), 5);
# }
# Ok::<(), trogon_object_id::ParseError>(())
```

The recipe owns its namespace, version and input contract. The attribute binds
the ID to that recipe without repeating namespace or template strings.
`from_identity` receives the recipe's associated input type and wraps its UUID
through the same checked constructor. Generated recipes can come from
`trogon_uuid_build`; protobuf descriptors remain a build-time dependency.

A binding selects the convenience constructor; it does not restrict the other
checked constructors to UUIDs from that recipe. UUID syntax alone cannot prove
which recipe produced a value. A template binding requires a UUID payload and
fails compilation when the feature, recipe trait or payload type is wrong.

## Advanced domain markers and the core

Direct ID structs need no separate domain marker or `type` alias.
`ObjectIdDomain` remains available when generic code needs a zero-sized
marker plus `ObjectId<Marker>` alias. It accepts `payload = Type` because the
marker has no payload field. With `template` enabled it also accepts
`template = Recipe` alongside `payload = Uuid`.

The `macros` feature is optional. An ordinary `Domain` implementation provides
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
declare `const _: Definition = DomainName::DEFINITION`. Handwritten enum parsers
can call `assert_disjoint` in a `const` assertion for the same prefix checks.

## Performance and compatibility

Domain markers and static trait dispatch add no stored domain data, vtables or
runtime registry. `ObjectId` has the same layout as its payload. Text parsed
from `&str` allocates once; owned text is validated and moved. Cloning text
copies its string, while UUID IDs are `Copy`. Derives generate implementations
during compilation and introduce no runtime registry. Validation, entropy
acquisition, formatting work and enum discriminators still have real costs.

This crate preserves the upstream prefixed wire format, configurable separators,
text validation and typed enums. It strengthens raw-value bounds,
control-character handling, declaration validation and enum prefix collision checks.
Compact UUIDs canonicalize text. Ecto and Phoenix integrations remain specific
to Elixir. This runtime has no protobuf dependency; `trogon_uuid_build` reads
UUID options from protobuf descriptors during code generation.

The default build has no optional features. `macros` enables `ObjectId` for
structs and enums, and `ObjectIdDomain` for advanced domain markers.
`serde` enables JSON-compatible traits and raw field adapters. `uuid` enables
compact UUID payloads and generation.
`template` binds UUID IDs to compiled `trogon_uuid` recipes and enables `uuid`.
The implementation forbids unsafe Rust.

If you used the unreleased `derive` feature, change it to `macros` in
`Cargo.toml`. Rust `#[derive(...)]` syntax and the `trogon_object_id_derive`
companion crate name stay the same.

## License

Licensed under [Apache-2.0](LICENSE).
