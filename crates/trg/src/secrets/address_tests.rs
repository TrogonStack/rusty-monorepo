use super::*;

fn keychain(path: &str, key: &str) -> SecretAddress {
    SecretAddress::Keychain(KeychainReference::new(
        SecretPath::parse(path).expect("path"),
        SecretKey::parse(key).expect("key"),
    ))
}

#[test]
fn an_address_knows_the_only_backend_kind_that_can_read_it() {
    assert_eq!(keychain("a", "k").backend_kind(), BackendKind::Keychain);
    assert_eq!(
        SecretAddress::Openbao(OpenbaoReference::new(
            SecretPath::parse("a").unwrap(),
            SecretKey::parse("k").unwrap()
        ))
        .backend_kind(),
        BackendKind::Openbao
    );
    assert_eq!(
        SecretAddress::OnePassword(Box::new(OnePasswordReference::parse("op://Ops/item/FIELD").unwrap()))
            .backend_kind(),
        BackendKind::OnePassword
    );
}

/// The rendering is what an error about a missing value shows, so a
/// 1Password var must read back as the reference that was written rather
/// than as coordinates nobody typed.
#[test]
fn an_address_renders_the_way_it_was_written() {
    assert_eq!(keychain("mcp/demo", "token").to_string(), "`token` at `mcp/demo`");
    assert_eq!(
        SecretAddress::OnePassword(Box::new(OnePasswordReference::parse("op://Ops/deploy/TOKEN").unwrap())).to_string(),
        "`op://Ops/deploy/TOKEN`"
    );
}
