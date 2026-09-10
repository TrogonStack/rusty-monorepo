//! How a config var addresses one value, in the vocabulary of the backend it
//! names.
//!
//! There is deliberately no shared `{ path, key }` spelling here. A vocabulary
//! invented once and imposed on every backend reads as native to none of them:
//! 1Password's own addressing is a secret reference, `op://<vault>/<item>/<field>`,
//! which is what its `Copy Secret Reference` button yields and what a user
//! already has in the clipboard. Making each kind own its address means a var
//! is written the way the product it addresses documents it.
//!
//! Keychain and OpenBao references are both path-and-key shaped today and are
//! still separate types, because they are not the same thing: an OpenBao path
//! is joined onto a mount, a prefix and an owner, while a keychain path maps
//! onto a service and account pair. Keeping them apart is what makes
//! [`SecretAddress`] exhaustive in the way that matters: a new backend cannot
//! compile until it has declared how it is addressed.

use std::fmt;

use super::keychain::KeychainReference;
use super::onepassword::OnePasswordReference;
use super::openbao::OpenbaoReference;
use super::{BackendKind, SecretKey, SecretPath};

/// One value's address, in the form its backend understands.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SecretAddress {
    Keychain(KeychainReference),
    Openbao(OpenbaoReference),
    /// Boxed because a secret reference carries three or four owned segments
    /// against the other kinds' two, and this address travels by value inside
    /// every `SecretVar`, which in turn sits inside the error types every read
    /// returns. Paying for the widest variant everywhere would make the happy
    /// path carry the cost of the rarest one. `clippy::result_large_err` is
    /// what holds the line: unboxed, this widens `SecretVar`, which widens
    /// the `VarError` every read returns past the lint's threshold.
    OnePassword(Box<OnePasswordReference>),
}

impl SecretAddress {
    /// The kind of backend this address can be read from.
    ///
    /// Config load is what pairs an address with the backend a var names, so
    /// by the time one exists the two already agree. What this answers is the
    /// question that follows from that: what a value at this address supports,
    /// such as whether `trg` can write to it.
    pub fn backend_kind(&self) -> BackendKind {
        match self {
            Self::Keychain(_) => BackendKind::Keychain,
            Self::Openbao(_) => BackendKind::Openbao,
            Self::OnePassword(_) => BackendKind::OnePassword,
        }
    }

    /// The path-and-key coordinates, for the surfaces that only speak those:
    /// `trg secret put`, and the `trg secret put` line an error offers as the
    /// fix. `None` for an address written in another vocabulary, which is not
    /// a translation those callers can do.
    pub fn path_key(&self) -> Option<(&SecretPath, &SecretKey)> {
        match self {
            Self::Keychain(r) => Some((r.path(), r.key())),
            Self::Openbao(r) => Some((r.path(), r.key())),
            Self::OnePassword(_) => None,
        }
    }
}

impl fmt::Display for SecretAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Keychain(r) => write!(f, "`{}` at `{}`", r.key(), r.path()),
            Self::Openbao(r) => write!(f, "`{}` at `{}`", r.key(), r.path()),
            Self::OnePassword(r) => write!(f, "`{r}`"),
        }
    }
}

#[cfg(test)]
mod tests {
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
            SecretAddress::OnePassword(Box::new(OnePasswordReference::parse("op://Ops/deploy/TOKEN").unwrap()))
                .to_string(),
            "`op://Ops/deploy/TOKEN`"
        );
    }
}
