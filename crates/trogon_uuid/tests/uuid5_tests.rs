use trogon_uuid::{uuid5, uuid5_parts, Namespace, Uuid};

#[test]
fn uuid5_matches_the_standard_dns_vector() {
    let value = uuid5(&Uuid::NAMESPACE_DNS, b"example.org");
    assert_eq!(
        value.to_string(),
        "aad03681-8b63-5304-89e0-8ca8f49461b5",
        "the published name-based UUID vector must match"
    );
    assert_eq!(value.get_version_num(), 5, "the result uses UUID version 5");
}

#[test]
fn ordered_pieces_match_the_standard_contiguous_algorithm() {
    for parts in [
        vec!["", ""],
        vec!["item/", "alpha", "/", "42"],
        vec!["🦀", "/", "日本語", "/", "🦀"],
        vec!["repeated", "repeated", ""],
    ] {
        let contiguous = parts.concat();
        let expected = Uuid::new_v5(&Uuid::NAMESPACE_URL, contiguous.as_bytes());
        assert_eq!(
            uuid5_parts(&Uuid::NAMESPACE_URL, &parts),
            expected,
            "streamed names retain their exact bytes and order"
        );
    }
    assert_eq!(
        uuid5_parts(&Uuid::NAMESPACE_DNS, std::iter::empty::<&[u8]>()),
        Uuid::new_v5(&Uuid::NAMESPACE_DNS, b""),
        "an empty iterator represents the empty name"
    );
}

#[test]
fn namespace_resolution_and_version_names_are_exact() {
    let dns = Namespace::Dns("example.com").resolve();
    assert_eq!(
        dns.to_string(),
        "cfbff0d1-9375-5685-968c-48ce8b15ae17",
        "DNS namespaces use the well-known DNS namespace"
    );
    let url = Namespace::Url("https://example.com/items").resolve();
    assert_eq!(
        url,
        Uuid::new_v5(&Uuid::NAMESPACE_URL, b"https://example.com/items"),
        "URL namespaces use the exact URL text"
    );
    assert_eq!(Namespace::Uuid(dns).resolve(), dns, "direct UUID namespaces remain unchanged");
    let version = Namespace::Uuid(dns).version_namespace("IDENTITY_VERSION_V1");
    assert_eq!(
        version,
        Uuid::new_v5(&dns, b"IDENTITY_VERSION_V1"),
        "identity versions hash their enum value names"
    );
    assert_ne!(
        version,
        Namespace::Uuid(dns).version_namespace("1"),
        "the numeric enum value is not its identity name"
    );
    assert_ne!(
        Namespace::Dns("EXAMPLE.com").resolve(),
        dns,
        "namespace text is never normalized implicitly"
    );
}
