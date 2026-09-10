//! Reading config vars out of the backends they address.
//!
//! Grouped by round trip before anything is read, because a backend answers
//! with more than one value at a time: a KV v2 read answers with the whole map
//! at a path, and `op item get` answers with every field of an item. Several
//! vars naming the same entry therefore cost one round trip between them, and
//! a server that pulls four values out of one entry does not pay for four.
//!
//! Addresses are already valid by the time they reach here: a var carries a
//! [`SecretAddress`] parsed at config load, in the vocabulary of the backend
//! it names, so nothing in this module re-checks a spelling.

use std::collections::{BTreeMap, HashMap};

use crate::config::{FetchedSecrets, SecretVar};

use super::onepassword::{OnePasswordItem, OnePasswordReference};
use super::{Backend, BackendError, Registry, SecretAddress, SecretKey, SecretPath, SecretsError};

#[derive(Debug, thiserror::Error)]
pub enum VarFetchError {
    #[error("var {var} names no usable backend: {cause}")]
    Backend {
        var: SecretVar,
        #[source]
        cause: Box<BackendError>,
    },

    #[error("var {var} could not be read: {cause}")]
    Read {
        var: SecretVar,
        #[source]
        cause: Box<SecretsError>,
    },

    /// Split from [`VarFetchError::MissingKey`] because the remedies differ: a
    /// path that holds nothing has never been written, while a path that holds
    /// the wrong keys usually means a typo in one of them.
    #[error("var {var} found nothing at that path; write it with `{command}`")]
    MissingPath { var: SecretVar, command: String },

    /// The same, for a backend `trg` cannot write to. There is no command to
    /// offer, and offering one that would only fail sends someone to fix the
    /// wrong thing.
    #[error("var {var} found no such item; create it in 1Password first")]
    MissingItem { var: SecretVar },

    #[error("var {var} found no such key there (that entry holds: {present})")]
    MissingKey { var: SecretVar, present: String },
}

/// Read every secret the given vars name, one round trip per distinct entry.
pub async fn fetch(registry: &Registry, wanted: &[SecretVar]) -> Result<FetchedSecrets, VarFetchError> {
    fetch_with(|name| registry.resolve(name), wanted).await
}

/// The same, against any way of naming a backend, so the grouping can be held
/// to its round-trip count without a reachable instance.
async fn fetch_with<F>(resolve: F, wanted: &[SecretVar]) -> Result<FetchedSecrets, VarFetchError>
where
    F: Fn(&str) -> Result<Backend, BackendError>,
{
    let mut out = FetchedSecrets::new();
    if wanted.is_empty() {
        return Ok(out);
    }

    // Sorted rather than hashed, so a config with two unreachable backends
    // fails on the same one every run. Split by what a read answers with
    // rather than merged behind one key, so that selecting a value out of an
    // answer never has to consider a shape the group cannot hold.
    let mut at_path: BTreeMap<(&str, &SecretPath), Vec<(&SecretVar, &SecretKey)>> = BTreeMap::new();
    let mut in_item: BTreeMap<(&str, &OnePasswordItem), Vec<(&SecretVar, &OnePasswordReference)>> = BTreeMap::new();
    for var in wanted {
        match var.address() {
            SecretAddress::Keychain(r) => at_path
                .entry((var.backend(), r.path()))
                .or_default()
                .push((var, r.key())),
            SecretAddress::Openbao(r) => at_path
                .entry((var.backend(), r.path()))
                .or_default()
                .push((var, r.key())),
            SecretAddress::OnePassword(r) => in_item
                .entry((var.backend(), r.item()))
                .or_default()
                .push((var, r.as_ref())),
        }
    }

    // A backend is built once per process even when several entries address it.
    let mut built: HashMap<String, Backend> = HashMap::new();

    for ((backend_name, path), vars) in at_path {
        let representative = vars[0].0;
        let backend = build(&resolve, &mut built, backend_name, representative)?;

        let map = backend
            .get(path)
            .await
            .map_err(|cause| read_error(representative, cause))?
            .ok_or_else(|| missing_entry(representative))?;

        for (var, key) in vars {
            // Key names are not secret; the values behind them are, and none
            // of them is named here.
            let value = map.get(key).ok_or_else(|| VarFetchError::MissingKey {
                var: var.clone(),
                present: map
                    .sorted_keys()
                    .iter()
                    .map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            })?;
            out.insert(var.clone(), value.clone());
        }
    }

    for ((backend_name, item), vars) in in_item {
        let representative = vars[0].0;
        let backend = build(&resolve, &mut built, backend_name, representative)?;

        let fields = backend
            .get_item(item)
            .await
            .map_err(|cause| read_error(representative, cause))?
            .ok_or_else(|| missing_entry(representative))?;

        for (var, reference) in vars {
            let value = fields
                .get(reference)
                .map_err(|cause| read_error(var, cause))?
                .ok_or_else(|| VarFetchError::MissingKey {
                    var: var.clone(),
                    present: fields.addresses().join(", "),
                })?;
            out.insert(var.clone(), value.clone());
        }
    }

    Ok(out)
}

/// The named backend, built at most once however many entries address it.
///
/// Handed back by clone rather than by reference because the two grouping
/// passes both need it and a `Backend` is a handle: cloning one copies the
/// settings, not the session or the token behind them.
fn build<F>(
    resolve: &F,
    built: &mut HashMap<String, Backend>,
    name: &str,
    representative: &SecretVar,
) -> Result<Backend, VarFetchError>
where
    F: Fn(&str) -> Result<Backend, BackendError>,
{
    if let Some(backend) = built.get(name) {
        return Ok(backend.clone());
    }
    let backend = resolve(name).map_err(|cause| VarFetchError::Backend {
        var: representative.clone(),
        cause: Box::new(cause),
    })?;
    built.insert(name.to_string(), backend.clone());
    Ok(backend)
}

fn read_error(var: &SecretVar, cause: SecretsError) -> VarFetchError {
    VarFetchError::Read {
        var: var.clone(),
        cause: Box::new(cause),
    }
}

/// Nothing at all is stored where this var points.
fn missing_entry(var: &SecretVar) -> VarFetchError {
    match var.put_command() {
        Some(command) => VarFetchError::MissingPath {
            var: var.clone(),
            command,
        },
        None => VarFetchError::MissingItem { var: var.clone() },
    }
}

#[cfg(test)]
mod tests {
    use secrecy::{ExposeSecret, SecretString};

    use super::*;
    use crate::secrets::fake::FakeBackend;
    use crate::secrets::openbao::OpenbaoReference;
    use crate::secrets::SecretMap;

    fn var(backend: &str, path: &str, key: &str) -> SecretVar {
        SecretVar::new(
            backend.to_string(),
            SecretAddress::Openbao(OpenbaoReference::new(
                SecretPath::parse(path).expect("path"),
                SecretKey::parse(key).expect("key"),
            )),
        )
    }

    /// A var addressed the way 1Password is addressed, over the same stand-in
    /// backend: the fake answers an item read out of the entry at
    /// `"<vault>/<item>"`, so the round-trip count is comparable.
    fn op_var(backend: &str, reference: &str) -> SecretVar {
        SecretVar::new(
            backend.to_string(),
            SecretAddress::OnePassword(Box::new(OnePasswordReference::parse(reference).expect("reference"))),
        )
    }

    async fn seeded(entries: &[(&str, &[(&str, &str)])]) -> FakeBackend {
        let backend = FakeBackend::new();
        for (path, pairs) in entries {
            let mut map = SecretMap::new();
            for (k, v) in *pairs {
                map.insert(SecretKey::parse(k).unwrap(), SecretString::from((*v).to_string()));
            }
            backend.set(&SecretPath::parse(path).unwrap(), &map).await.unwrap();
        }
        backend
    }

    /// The whole reason to address a secret as path plus key: an entry answers
    /// with every key it holds, so vars that share a path share the read.
    #[tokio::test]
    async fn vars_sharing_a_path_cost_one_read_between_them() {
        let fake = seeded(&[("agentgateway", &[("token", "t"), ("principal", "p")])]).await;
        let backend = Backend::Fake(fake.clone());

        let wanted = vec![
            var("homelab", "agentgateway", "token"),
            var("homelab", "agentgateway", "principal"),
        ];
        let fetched = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap();

        assert_eq!(fetched.len(), 2);
        assert_eq!(
            fetched.get(&wanted[0]).unwrap().expose_secret(),
            "t",
            "the first var reads its own key, not the other one's"
        );
        assert_eq!(fetched.get(&wanted[1]).unwrap().expose_secret(), "p");
        assert_eq!(fake.get_count(), 1, "two vars at one path is one round trip");
    }

    #[tokio::test]
    async fn distinct_paths_are_read_separately() {
        let fake = seeded(&[("one", &[("k", "1")]), ("two", &[("k", "2")])]).await;
        let backend = Backend::Fake(fake.clone());

        let wanted = vec![var("homelab", "one", "k"), var("homelab", "two", "k")];
        fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap();

        assert_eq!(fake.get_count(), 2);
    }

    /// Two servers declaring the same secret is the point of giving it an
    /// identity of its own, so it must not cost twice.
    #[tokio::test]
    async fn the_same_var_named_twice_is_read_once() {
        let fake = seeded(&[("agentgateway", &[("token", "t")])]).await;
        let backend = Backend::Fake(fake.clone());

        let wanted = vec![
            var("homelab", "agentgateway", "token"),
            var("homelab", "agentgateway", "token"),
        ];
        let fetched = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap();

        assert_eq!(fetched.len(), 1);
        assert_eq!(fake.get_count(), 1);
    }

    #[tokio::test]
    async fn nothing_wanted_reaches_nothing() {
        let fake = FakeBackend::new();
        let backend = Backend::Fake(fake.clone());

        let fetched = fetch_with(|_| Ok(backend.clone()), &[]).await.unwrap();

        assert!(fetched.is_empty());
        assert_eq!(
            fake.get_count(),
            0,
            "a server with no secret vars must not touch a backend"
        );
    }

    /// An entry that was never written and one that lacks the key are separate
    /// mistakes, and the first is the one with a command to fix it.
    #[tokio::test]
    async fn an_unwritten_path_names_the_command_that_writes_it() {
        let fake = seeded(&[]).await;
        let backend = Backend::Fake(fake);

        let wanted = vec![var("homelab", "agentgateway", "token")];
        let err = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap_err();

        let msg = err.to_string();
        assert!(matches!(err, VarFetchError::MissingPath { .. }), "{msg}");
        assert!(msg.contains("trg secret put"), "{msg}");
        assert!(msg.contains("--key token"), "{msg}");
    }

    #[tokio::test]
    async fn a_missing_key_names_the_keys_that_are_there() {
        let fake = seeded(&[("agentgateway", &[("token", "t"), ("principal", "p")])]).await;
        let backend = Backend::Fake(fake);

        let wanted = vec![var("homelab", "agentgateway", "tokne")];
        let err = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap_err();

        let msg = err.to_string();
        assert!(matches!(err, VarFetchError::MissingKey { .. }), "{msg}");
        assert!(msg.contains("principal") && msg.contains("token"), "{msg}");
    }

    /// The keys are named to catch a typo; the values behind them must not
    /// travel along with that.
    #[tokio::test]
    async fn a_missing_key_does_not_name_any_value() {
        let fake = seeded(&[("agentgateway", &[("token", "super-secret-value")])]).await;
        let backend = Backend::Fake(fake);

        let wanted = vec![var("homelab", "agentgateway", "nope")];
        let err = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap_err();

        assert!(!err.to_string().contains("super-secret-value"), "{err}");
    }

    /// The same batching property, in 1Password's own vocabulary: an
    /// `op item get` answers with every field of the item, so four references
    /// into one item must not become four subprocesses.
    #[tokio::test]
    async fn references_into_one_item_cost_one_read_between_them() {
        let fake = seeded(&[("Ops/deploy", &[("TOKEN", "t"), ("Prod/TOKEN", "p")])]).await;
        let backend = Backend::Fake(fake.clone());

        let wanted = vec![
            op_var("op", "op://Ops/deploy/TOKEN"),
            op_var("op", "op://Ops/deploy/Prod/TOKEN"),
        ];
        let fetched = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap();

        assert_eq!(fetched.get(&wanted[0]).unwrap().expose_secret(), "t");
        assert_eq!(
            fetched.get(&wanted[1]).unwrap().expose_secret(),
            "p",
            "a sectioned reference must not pick up the unsectioned field of the same name"
        );
        assert_eq!(fake.get_count(), 1, "two references into one item is one round trip");
    }

    /// 1Password items are managed in 1Password, so there is no
    /// `trg secret put` to offer and the error must not invent one.
    #[tokio::test]
    async fn a_missing_item_offers_no_write_command() {
        let fake = seeded(&[]).await;
        let backend = Backend::Fake(fake);

        let wanted = vec![op_var("op", "op://Ops/deploy/TOKEN")];
        let err = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap_err();

        let msg = err.to_string();
        assert!(matches!(err, VarFetchError::MissingItem { .. }), "{msg}");
        assert!(!msg.contains("trg secret put"), "{msg}");
    }

    #[tokio::test]
    async fn a_reference_to_no_such_field_names_the_fields_that_are_there() {
        let fake = seeded(&[("Ops/deploy", &[("TOKEN", "t"), ("Prod/OTHER", "o")])]).await;
        let backend = Backend::Fake(fake);

        let wanted = vec![op_var("op", "op://Ops/deploy/TOEKN")];
        let err = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap_err();

        let msg = err.to_string();
        assert!(matches!(err, VarFetchError::MissingKey { .. }), "{msg}");
        assert!(msg.contains("TOKEN") && msg.contains("Prod/OTHER"), "{msg}");
    }

    #[tokio::test]
    async fn a_backend_that_cannot_be_named_reports_the_var_that_named_it() {
        let wanted = vec![var("nosuch", "agentgateway", "token")];
        let err = fetch_with(
            |name| {
                Err(BackendError::Unknown {
                    name: name.to_string(),
                    declared: "homelab".to_string(),
                })
            },
            &wanted,
        )
        .await
        .unwrap_err();

        let msg = err.to_string();
        assert!(matches!(err, VarFetchError::Backend { .. }), "{msg}");
        assert!(msg.contains("nosuch"), "{msg}");
    }
}
