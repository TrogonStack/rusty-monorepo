#![no_std]

pub fn exercise() -> bool {
    let contiguous = recipes::uuid5(&recipes::Uuid::NAMESPACE_DNS, b"example.org");
    let streamed = recipes::uuid5_parts(&recipes::Uuid::NAMESPACE_DNS, ["example", ".", "org"]);
    let versioned = recipes::Namespace::Dns("example.com").version_namespace("IDENTITY_VERSION_V1");
    contiguous == streamed
        && contiguous.as_u128() == 0xaad036818b63530489e08ca8f49461b5
        && versioned.as_u128() == 0xd18871f86a5458e39793703aa19d48d4
}

#[cfg(test)]
mod tests {
    #[test]
    fn public_vectors() {
        assert!(super::exercise(), "deterministic public vectors");
    }
}
