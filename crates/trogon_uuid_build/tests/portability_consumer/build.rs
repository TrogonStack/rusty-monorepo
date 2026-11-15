fn main() {
    let directory = std::env::var_os("OUT_DIR").expect("Cargo output directory");
    let output = std::path::PathBuf::from(directory).join("recipes.rs");
    builder::Config::new()
        .runtime_path("::recipes")
        .write(
            include_bytes!("../fixtures/recipes.binpb"),
            &[
                builder::Template::new(
                    "example.catalog.v1.SkuIdentityVersion",
                    "SKU_IDENTITY_VERSION_V1",
                    "SkuIdentityV1",
                ),
                builder::Template::new(
                    "example.catalog.v1.SkuIdentityVersion",
                    "STATIC_TEMPLATE",
                    "StaticIdentity",
                ),
                builder::Template::new(
                    "example.catalog.v1.SkuIdentityVersion",
                    "EMPTY_TEMPLATE",
                    "EmptyIdentity",
                ),
            ],
            output,
        )
        .expect("public recipe descriptors");
    println!("cargo:rerun-if-changed=../fixtures/recipes.binpb");
}
