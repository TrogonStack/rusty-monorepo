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
use trogon_object_id::{ObjectId, Uuid, Uuid7};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ObjectId)]
#[object_id(prefix = "user")]
#[repr(transparent)]
pub struct UserId(Uuid);

let user = UserId::generate::<Uuid7>()?;
let encoded = user.to_string();
let restored = UserId::try_from(encoded)?;
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

For spelling-preserving text, use `Text` or `IntegerText`:

```rust
# #[cfg(feature = "macros")]
# {
use trogon_object_id::{IntegerText, ObjectId};

#[derive(ObjectId)]
#[object_id(prefix = "invoice", separator = ":")]
#[repr(transparent)]
struct InvoiceId(IntegerText);

let raw = String::from("+00042");
let original_buffer = raw.as_ptr();
let payload = IntegerText::try_from(raw)?;
let invoice = InvoiceId::try_from_payload(payload)?;
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
`try_from_payload` validates an existing typed payload before wrapping it.
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

A custom payload can reuse checked text and add a rule in `TryFrom`. `From` is
reserved for conversions that cannot fail, such as recovering the underlying
text or wrapping an existing ID in an enum.

```rust
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

impl std::fmt::Display for AsciiToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
# Ok::<(), ParseError>(())
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

The `uuid` feature provides `Uuid`, `Uuid4`, and `Uuid7`. Generic generation
requires a generator with the domain's exact payload type. A generator cannot
silently substitute text for a UUID or bypass payload validation.

Compact UUID payloads accept hyphenated text and display canonical lowercase
hyphenated text. UUID text capitalization is normalized. To preserve exact
spelling instead, choose `UuidText` and construct it with `TryFrom`.
That choice intentionally keeps owned text allocation and cloning costs.

UUID parsing, copying and formatting into an existing buffer need no heap
allocation in this crate. UUID generation uses the `uuid` crate's system
randomness and time behavior; its randomness failure can panic, even though the
shared generation API returns `Result` for fallible custom generators. UUID
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

The default build has no optional features. `macros` enables the `ObjectId`
derive for structs and enums.
`serde` enables JSON-compatible traits and raw field adapters. `uuid` enables
compact UUID payloads and generation.
The implementation forbids unsafe Rust.

## License

Licensed under [Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0).
