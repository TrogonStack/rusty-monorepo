use std::path::PathBuf;

use trogon_uuid_build::{Config, Template};

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "Cargo build scripts send dependency-tracking directives through stdout."
    )
)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = "../../crates/trogon_uuid_build/tests/fixtures/recipes.binpb";
    println!("cargo:rerun-if-changed={fixture}");
    let descriptors = std::fs::read(fixture)?;
    let output = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("OUT_DIR is required")?);
    let templates = [
        Template::new(
            "example.catalog.v1.SkuIdentityVersion",
            "SKU_IDENTITY_VERSION_V1",
            "SkuIdentityV1",
        ),
        Template::new(
            "example.catalog.v1.SkuIdentityVersion",
            "SKU_IDENTITY_VERSION_V2",
            "SkuIdentityV2",
        ),
        Template::new("example.catalog.v1.SkuIdentityVersion", "DIRECT_UUID", "DirectIdentity"),
        Template::new("example.catalog.v1.SkuIdentityVersion", "URL_NAMESPACE", "UrlIdentity"),
        Template::new(
            "example.catalog.v1.SkuIdentityVersion",
            "STATIC_TEMPLATE",
            "StaticIdentity",
        ),
        Template::new(
            "example.catalog.v1.SkuIdentityVersion",
            "EMPTY_TEMPLATE",
            "EmptyIdentity",
        ),
        Template::new(
            "example.catalog.v1.SkuIdentityVersion",
            "REPEATED_TEMPLATE",
            "RepeatedIdentity",
        ),
        Template::new(
            "example.catalog.v1.MissingNamespaceVersion",
            "OVERRIDE_WITHOUT_DEFAULT",
            "OverrideIdentity",
        ),
    ];
    Config::new().write(&descriptors, &templates, output.join("identities.rs"))?;
    Ok(())
}
