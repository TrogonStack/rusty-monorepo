# trogon_uuid_build

Generate typed Rust UUIDv5 recipes from protobuf descriptor options during the
build. Applications use the generated marker and input types, without loading
protobuf descriptors or parsing templates at runtime.

## Build integration

Add this package as a build dependency and `trogon_uuid` as a normal dependency.
Produce a `FileDescriptorSet` with imports included, so it contains the public
`trogon.uuid.v1` extension definitions as well as the selected application enum.
The generator does not invoke `protoc`, fetch schemas, or modify descriptors.

```rust,no_run
use std::path::PathBuf;
use trogon_uuid_build::{Config, Template};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let descriptors = std::fs::read("proto/identity.binpb")?;
    let output = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("OUT_DIR is required")?);
    Config::new().write(
        &descriptors,
        &[Template::new(
            "example.catalog.v1.SkuIdentityVersion",
            "SKU_IDENTITY_VERSION_V1",
            "SkuIdentityV1",
        )],
        output.join("identities.rs"),
    )?;
    println!("cargo:rerun-if-changed=proto/identity.binpb");
    Ok(())
}
```

Include the generated file from your application with
`include!(concat!(env!("OUT_DIR"), "/identities.rs"))`. A recipe with named
placeholders generates a stateless marker such as `SkuIdentityV1` and an input
struct such as `SkuIdentityV1Input<'a>`. Each required field is a borrowed
`&'a str`. Missing or misspelled inputs fail Rust compilation.

Call `SkuIdentityV1::uuid5(SkuIdentityV1Input { ... })` to obtain a typed `Uuid`.
The marker also implements `trogon_uuid::UuidTemplate` for object-ID template
bindings. Its `NAMESPACE` constant contains the resolved version namespace.
A template without placeholders uses `()` as input and embeds its final UUID
in a `VALUE` constant. An empty template is also a valid constant recipe; an
absent `format` option is an error.

`Config::runtime_path("::uuid_runtime")` supports a renamed runtime dependency.
The path and generated type names are parsed as Rust syntax before emission.
A missing enum, unconfigured enum value, invalid UUID, ambiguous generated name,
unsupported option schema, or malformed template returns `BuildError`.

## Identity compatibility

The supported options are the public
[`trogon.uuid.v1` schema](https://github.com/TrogonStack/trogon-proto/tree/0768ec5038540a5f503b6f898ec70120fde81766/proto/trogon/uuid/v1).
The selected enum value's namespace overrides the enum-level default; absence
of both is an error. An explicitly present but empty namespace is also an error.
A direct UUID is parsed as a UUID. DNS and URL namespace strings are hashed with
the corresponding well-known UUID namespace, preserving their exact UTF-8 text.

For compatibility with the published `UuidTemplate` recipe behavior, the
namespace is further derived using the exact protobuf enum value **name**:

```text
version namespace = UUIDv5(resolved namespace, enum value name)
identity = UUIDv5(version namespace, rendered template)
```

Renaming an enum value, changing namespace text, or changing template bytes
changes the generated identity. Use a new identity version for such changes.

Templates contain literals and `{field_name}` placeholders. Field names use
ASCII letters, digits, and underscores, and cannot start with a digit. Rust
keywords are emitted as raw identifiers when supported. Repeated placeholders
reuse the same input field while retaining their position in the rendered name.
Literal braces and malformed placeholders are rejected rather than silently
interpreted as identity text. Field values are not escaped or normalized.
Validate component values in the application so distinct logical inputs cannot
render the same name through separators inside a value.
The generated code hashes literal and value pieces in order without allocating
an intermediate concatenated name.

The generator cannot infer application-specific ID types from a protobuf
`string` field. An application adapter chooses the exact storage representation
for each typed component before supplying the recipe input. UUID generation
establishes deterministic identity, not authorization or secrecy.

## Fixtures

The checked descriptor fixture under `tests/fixtures` was generated with
`protoc` 36.2 and `--include_imports`, without source information. Its readable
schemas mirror the public extension wire contract and use generic example
names. Tests consume the fixture without an external
protobuf compiler or network access.

## License

Licensed under [Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0).
