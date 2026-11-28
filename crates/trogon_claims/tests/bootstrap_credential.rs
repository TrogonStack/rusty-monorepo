#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::marker::PhantomData;

use trogon_claims::BootstrapCredential;
use zeroize::ZeroizeOnDrop;

trait NotImplemented {
    const IMPLEMENTED: bool = false;
}

impl<T> NotImplemented for T {}

struct CloneProbe<T>(PhantomData<T>);

impl<T: Clone> CloneProbe<T> {
    const IMPLEMENTED: bool = true;
}

fn zeroizes_on_drop<T: ZeroizeOnDrop>() {}

#[test]
fn a_bootstrap_credential_cannot_be_cloned() {
    const { assert!(!<CloneProbe<BootstrapCredential>>::IMPLEMENTED) };
    const { assert!(<CloneProbe<String>>::IMPLEMENTED) };
}

#[cfg(feature = "serde")]
#[test]
fn a_bootstrap_credential_has_no_serde_impls() {
    struct SerializeProbe<T>(PhantomData<T>);
    impl<T: serde::Serialize> SerializeProbe<T> {
        #[allow(dead_code)]
        const IMPLEMENTED: bool = true;
    }
    const { assert!(!<SerializeProbe<BootstrapCredential>>::IMPLEMENTED) };
}

#[test]
fn debug_never_shows_the_secret() {
    let credential = BootstrapCredential::new("super-secret-token");
    let rendered = format!("{credential:?} {credential:#?}");
    assert!(!rendered.contains("super-secret-token"));
    assert!(rendered.contains("<redacted>"));
}

#[test]
fn the_secret_is_reachable_only_through_the_named_accessor() {
    let credential = BootstrapCredential::new("super-secret-token");
    assert_eq!(credential.expose_for_token_exchange(), "super-secret-token");
}

#[test]
fn a_bootstrap_credential_zeroizes_on_drop() {
    zeroizes_on_drop::<BootstrapCredential>();
}
