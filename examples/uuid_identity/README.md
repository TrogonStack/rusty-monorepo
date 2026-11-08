# Generated UUID identities

This workspace example compiles public protobuf UUID options into Rust recipes
in `build.rs`, then binds a recipe to an object-ID newtype. The example runtime
is `#![no_std]` and uses neither an allocator nor random or time generation.
The build script and generator remain host tools with `std` and filesystem I/O.

```rust
use uuid_identity_example::{generated::{SkuIdentityV1, SkuIdentityV1Input}, SkuId};

let id = SkuId::from(SkuIdentityV1::uuid5(SkuIdentityV1Input {
    warehouse_id: "warehouse-17",
    sku_code: "ABC-123",
}));
assert_eq!(id.to_string(), "sku_1724f7cd-15b8-5e76-ba00-70d188c9b209");
# Ok::<(), trogon_object_id::ParseError>(())
```

The ID declaration is a single newtype with a statically selected recipe:

```rust
use uuid_identity_example::generated;

#[derive(trogon_object_id::ObjectId)]
#[object_id(prefix = "sku", template = generated::SkuIdentityV1)]
#[repr(transparent)]
struct SkuId(trogon_object_id::Uuid);
```

The example uses the checked descriptor fixture from `trogon_uuid_build`.
No protobuf compiler or network access is needed to build it. In an application,
replace that input with your own `FileDescriptorSet` containing imports.
The generated marker's namespace is fixed at build time. Templates containing
fields hash borrowed input bytes at runtime. Static templates embed a final UUID
constant. The resulting object ID stores a 16-byte UUID; formatting its prefixed
text form with `to_string()` allocates a string.

Run `cargo test -p uuid_identity_example` to verify the generated recipes and
their object-ID integration against independent UUIDv5 vectors.

Licensed under [Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0).
