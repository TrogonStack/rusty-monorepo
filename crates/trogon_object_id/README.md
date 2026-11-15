# trogon_object_id

Domain-specific prefixed identifiers for Rust, inspired by
[`trogon_object_id` for Elixir](https://hex.pm/packages/trogon_object_id/0.3.0).
Declare ordinary Rust newtypes and enums, then derive their supporting
implementations. Payloads remain actual Rust types: UUID identifiers store a
`Uuid`, while text identifiers store validated `Text` or another payload newtype.

## Quick start

Enable the `macros` and `uuid` features for a compact UUID identifier:

```rust
# #[cfg(all(feature = "macros", feature = "uuid"))]
# {
use trogon_object_id::{ObjectId, Uuid};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ObjectId)]
#[object_id(prefix = "user")]
#[repr(transparent)]
pub struct UserId(Uuid);

let payload = Uuid::try_from("550e8400-e29b-41d4-a716-446655440000")
    .expect("valid UUID");
let user = UserId::from(payload);
let restored = UserId::try_from("user_550e8400-e29b-41d4-a716-446655440000")?;
assert_eq!(restored, user);
assert_eq!(core::mem::size_of::<UserId>(), 16);
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

Enable `alloc` for spelling-preserving `Text` or `IntegerText`:

```rust
# #[cfg(all(feature = "macros", feature = "alloc"))]
# {
use trogon_object_id::{IntegerText, ObjectId};

#[derive(ObjectId)]
#[object_id(prefix = "invoice", separator = ":")]
#[repr(transparent)]
struct InvoiceId(IntegerText);

let raw = String::from("+00042");
let original_buffer = raw.as_ptr();
let payload = IntegerText::try_from(raw)?;
let invoice = InvoiceId::from(payload);
assert_eq!(invoice.as_raw(), "+00042");
assert_eq!(invoice.as_raw().as_ptr(), original_buffer);
assert_eq!(invoice.to_string(), "invoice:+00042");
let raw = String::from(invoice.into_payload());
assert_eq!(raw.as_ptr(), original_buffer);
# }
# Ok::<(), trogon_object_id::ParseError>(())
```

## Enums of identifiers

Declare the variants as an ordinary Rust enum. The same `ObjectId` derive adds
prefixed parsing, `Display`, member conversions, and Serde implementations when the
`serde` feature is enabled. Standard derives such as `Debug` and `Clone` remain
explicit on the enum:

```rust
# #[cfg(all(feature = "macros", feature = "alloc"))]
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

let user = UserId::try_from("user_abc-123")?;
let principal = PrincipalId::from(user);
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
and 4 ASCII punctuation characters. The `ObjectId` derive checks these constraints
during compilation, even when the ID is unused.

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
Validated `Text`, `IntegerText`, `UuidText` and `Uuid` payloads convert to generic
and derived IDs with `From`. No text is inspected to guess whether it is raw or
prefixed: `TextId::from(Text::try_from("text_value")?)` retains `text_value` as
the raw suffix. Custom payloads use `#[object_id(prefix = "...", fallible)]`,
which derives `TryFrom<CustomPayload>` and calls `Payload::validate`.
The default derive delegates `From<Payload>` to the runtime's built-in checked
conversions; a custom payload cannot opt into an unchecked infallible path.
Aliases of built-in payloads work with the same trait rules.

For generic `ObjectId<D>` with a custom payload, use
`ObjectId::<D>::try_from(RawPayload(payload))`. This distinct typed source avoids
Rust's overlap between a generic `TryFrom<D::Payload>`, full-string conversions
and the standard blanket `TryFrom` implementation. `RawPayload` carries a
payload implementing `Payload`, not a string with inferred prefix semantics.
Existing inherent constructors remain compatibility helpers and apply the same
checks.
`as_raw` borrows the raw representation: `&str` for text or `&Uuid` for a compact
UUID. Both support `Display` without allocating a temporary string.

Raw values must contain between 1 and 1024 UTF-8 bytes and no Unicode control
characters. Text keeps its exact spelling, including Unicode and integer signs
or leading zeros. `IntegerText` validates signed ASCII decimal syntax without a
numeric range. These payloads implement `TryFrom<&str>`, `TryFrom<String>` and
`FromStr`. `From<Text> for String` and the corresponding conversions from
`IntegerText` return the existing storage without another allocation.

`Payload` is the extension point for other typed representations. Its
implementation is trusted to enforce immutable, valid state and the documented
text bounds on all parsing paths. Custom payload newtypes put their construction
rules in `TryFrom`, and their `Payload` parsers delegate to those conversions.
The built-in parsers reject malformed input before allocating owned storage.
Errors describe the failed constraint without retaining or displaying the input.

## Custom payload conversions

This owning example requires `alloc`.

A custom payload can reuse checked text and add a rule in `TryFrom`. `From` is
reserved for conversions that cannot fail, such as recovering the underlying
text or wrapping an existing ID in an enum.

```rust
# #[cfg(feature = "alloc")]
# {
use trogon_object_id::{ParseError, Payload, Text};

struct AsciiToken(Text);

impl TryFrom<Text> for AsciiToken {
    type Error = ParseError;

    fn try_from(value: Text) -> Result<Self, Self::Error> {
        if value.as_str().bytes().all(|byte| byte.is_ascii_lowercase()) {
            Ok(Self(value))
        } else {
            Err(ParseError::InvalidValue)
        }
    }
}

impl From<AsciiToken> for Text {
    fn from(value: AsciiToken) -> Self {
        value.0
    }
}

impl core::fmt::Display for AsciiToken {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Payload for AsciiToken {
    type Raw = str;

    fn parse(value: &str) -> Result<Self, ParseError> {
        Text::try_from(value)?.try_into()
    }

    fn parse_owned(value: String) -> Result<Self, ParseError> {
        Text::try_from(value)?.try_into()
    }

    fn validate(&self) -> Result<(), ParseError> {
        // Private immutable storage and TryFrom establish the rule once.
        Ok(())
    }

    fn as_raw(&self) -> &str {
        self.0.as_str()
    }
}

let token = AsciiToken::try_from(Text::try_from("abc")?)?;
let text = Text::from(token);
assert_eq!(String::from(text), "abc");
# }
# Ok::<(), trogon_object_id::ParseError>(())
```

This example validates the shared text bounds before applying its lowercase
rule, so malformed custom values can allocate a `Text` before being rejected.
An implementation can validate borrowed input before allocating when that cost
matters. Owned text retains its allocation through both conversions. Custom
payloads must keep their constructors and `Payload::validate` consistent;
unchecked or mutable state must be rejected when an ID wraps it.

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

The `uuid` feature provides compact `Uuid` representation and parsing without
randomness, time or an allocator. `alloc` plus `uuid` adds `UuidText`.
Enable the separate `uuid-generate` feature for `Uuid4` and `Uuid7`. Generic generation
requires a generator with the domain's exact payload type. A generator cannot
silently substitute text for a UUID or bypass payload validation.

Compact UUID payloads accept hyphenated text and display canonical lowercase
hyphenated text. UUID text capitalization is normalized. To preserve exact
spelling instead, choose `UuidText` and construct it with `TryFrom`.
That choice intentionally keeps owned text allocation and cloning costs.

UUID parsing, copying and formatting into an existing buffer need no heap
allocation in this crate. UUID generation uses the `uuid` crate's system
randomness and time behavior; its randomness failure can panic, even though the
shared generation API returns `Result` for fallible custom generators. The convenience generation feature enables `uuid/std` and its RNG backend; it
is not a core-only capability. Browser `wasm32-unknown-unknown` generation needs
an explicitly selected supported `uuid` randomness backend, such as its `js`
feature via a direct dependency. Other targets need their own supported entropy
and time backend. A successful target build alone does not prove host entropy
or clock behavior. UUID
version 7 exposes a timestamp. Identifiers do not establish authorization.

## Performance and compatibility

Generated implementations use static trait dispatch with no stored domain data,
vtables or runtime registry. ID structs have the same layout as their payloads.
Text parsed from `&str` allocates once; owned text is validated and moved. Cloning text
copies its string, while UUID IDs are `Copy`. Derives generate implementations
during compilation and introduce no runtime registry. Validation, entropy
acquisition, formatting work and enum discriminators still have real costs.

This crate preserves the upstream prefixed wire format, configurable separators,
text validation and typed enums. It strengthens raw-value bounds,
control-character handling, declaration validation and enum prefix collision checks.
Compact UUIDs canonicalize text. Ecto and Phoenix integrations remain specific
to Elixir. This runtime has no protobuf dependency.

The runtime is `#![no_std]` with `default = []`. Its baseline metadata, errors,
wrapper, borrowed parsing dispatch, formatting and custom compact payloads need
neither `std` nor a global allocator. Payload implementations and output sinks
retain responsibility for their own costs.

| Feature | Capability |
| --- | --- |
| `alloc` | Owned text payloads, String methods and conversions; application supplies an allocator |
| `macros` | Host derive tooling emitting core-only target APIs, with owned operations following runtime `alloc` |
| `serde` | Full-form and raw traits with borrowed string visitors, without enabling `alloc` |
| `uuid` | Binary UUID payload and parsing, without RNG or clock |
| `uuid-generate` | UUID v4/v7 convenience generation using platform randomness/time and `uuid/std` |

Existing text users must explicitly enable `alloc`; callers of `Uuid4`/`Uuid7`
must add `uuid-generate`. `Payload::parse_owned` and
`Identifier::try_from_owned` exist only with runtime `alloc`. Custom codecs
implementing those methods should forward their own owning feature to
`trogon_object_id/alloc` and gate the method with it. Derived code uses
runtime-defined feature macros, so application feature names need not match.

Serde serializers choose whether `collect_str` allocates. The core-only visitor
accepts borrowed or transient string callbacks; the owned string callback and
buffer reuse are available with `alloc`. Enabling `serde` alone does not promise
that an arbitrary serializer or custom payload never allocates.

The isolated consumer under `tests/portability_consumer` checks renamed runtime
dependencies and feature names independently of workspace feature unification.
Its core-only Wasm executable links without an application allocator. Features
unified by other workspace packages can enable `std` in shared dependencies;
inspect the final application's target graph when enforcing a core-only build.
The implementation forbids unsafe Rust.

## License

Licensed under [Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0).
