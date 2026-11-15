# trogon_uuid

Static UUIDv5 identity recipes with borrowed typed inputs. The crate reuses
[`uuid::Uuid`](https://docs.rs/uuid/latest/uuid/struct.Uuid.html) and has no
protobuf runtime dependency. The runtime is `#![no_std]` and requires neither
`std` nor a global allocator. UUID and hashing dependency defaults are disabled;
UUIDv5 enables no random or time generation backend. Host build tooling remains
a separate crate.

## Derive a UUID from exact bytes

```rust
use trogon_uuid::{uuid5, uuid5_parts, Uuid};

let contiguous = uuid5(&Uuid::NAMESPACE_DNS, b"example.org");
assert_eq!(contiguous.to_string(), "aad03681-8b63-5304-89e0-8ca8f49461b5");

let pieces = uuid5_parts(&Uuid::NAMESPACE_DNS, ["example", ".", "org"]);
assert_eq!(pieces, contiguous);
```

`uuid5_parts` feeds ordered pieces directly into SHA-1 and constructs the result
with the `uuid` crate's UUIDv5 builder. It does not concatenate the name into a
temporary string or allocate. The iterator and each piece may still have costs
chosen by the caller. This crate forbids unsafe Rust.

## Resolve a versioned namespace

```rust
use trogon_uuid::{Namespace, Uuid};

let namespace = Namespace::Dns("example.com").resolve();
let version = Namespace::Uuid(namespace).version_namespace("IDENTITY_VERSION_V1");
assert_eq!(version.to_string(), "d18871f8-6a54-58e3-9793-703aa19d48d4");
```

`Namespace::Uuid` accepts an existing namespace UUID. `Dns` and `Url` derive a
namespace using the respective well-known UUID namespace and exact UTF-8 text.
They do not resolve names over the network, normalize case, canonicalize URLs,
or validate DNS and URL syntax.

The version name is the exact enum value name, not its numeric value. For
compatibility with the public Trogon UUID template convention, an identity is
`UUIDv5(UUIDv5(resolved namespace, version name), rendered template)`.
Changing the namespace text, version name, or rendered bytes changes identity.
No escaping, separators, or Unicode normalization are inserted automatically.

## Use a compiled recipe

The build-time adapter `trogon_uuid_build` reads protobuf descriptors and emits
ordinary Rust types implementing `UuidTemplate`. Generated recipes embed their
resolved namespaces and render their inputs without a runtime template parser.
The same trait also supports handwritten recipes:

```rust
use trogon_uuid::{uuid5_parts, Uuid, UuidTemplate};

struct ItemIdentityV1;

struct ItemInput<'a> {
    code: &'a str,
    number: &'a str,
}

impl UuidTemplate for ItemIdentityV1 {
    type Input<'a> = ItemInput<'a>;
    const NAMESPACE: Uuid = Uuid::from_u128(0xd18871f86a5458e39793703aa19d48d4);

    fn uuid5(input: Self::Input<'_>) -> Uuid {
        uuid5_parts(&Self::NAMESPACE, ["item/", input.code, "/", input.number])
    }
}

let id = ItemIdentityV1::uuid5(ItemInput { code: "alpha", number: "42" });
assert_eq!(id.to_string(), "fc67f55b-4fbb-53b1-8fac-c598af79b6d1");
```

Input fields are ordinary Rust fields, so missing fields and incompatible types
are compilation errors. Field contents still come from runtime data and need
whatever validation the application requires. An object-ID wrapper can hold
the generated `Uuid` without converting through a string.

Implementations of `UuidTemplate` are trusted application code. A `Uuid` alone
does not prove which recipe produced it. UUIDv5 uses SHA-1 for deterministic
identity and is unsuitable as a credential, secret, or authentication proof.

## License

Licensed under [Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0).
