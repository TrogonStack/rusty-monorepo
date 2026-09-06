//! Reading `{ backend, path, key }` config vars out of their backends.
//!
//! Grouped by backend and path before anything is read, because a KV v2 read
//! answers with the whole map at a path rather than one value in it. Several
//! vars naming the same path therefore cost one round trip between them, and a
//! server that pulls four values out of one entry does not pay for four.

use std::collections::{BTreeMap, HashMap};

use crate::config::{FetchedSecrets, SecretVar};

use super::{Backend, BackendError, KeyError, PathError, Registry, SecretKey, SecretPath, SecretsError};

#[derive(Debug, thiserror::Error)]
pub enum VarFetchError {
    #[error("var {var} names no usable backend: {cause}")]
    Backend {
        var: SecretVar,
        #[source]
        cause: Box<BackendError>,
    },

    #[error("var {var} names an unusable path: {cause}")]
    Path {
        var: SecretVar,
        #[source]
        cause: PathError,
    },

    #[error("var {var} names an unusable key: {cause}")]
    Key {
        var: SecretVar,
        #[source]
        cause: KeyError,
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

    #[error("var {var} found no such key there (that entry holds: {present})")]
    MissingKey { var: SecretVar, present: String },
}

/// Read every secret the given vars name, one round trip per distinct path.
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
    // fails on the same one every run.
    let mut groups: BTreeMap<(&str, &str), Vec<&SecretVar>> = BTreeMap::new();
    for var in wanted {
        groups.entry((&var.backend, &var.path)).or_default().push(var);
    }

    // A backend is built once per process even when several paths address it.
    let mut built: HashMap<&str, Backend> = HashMap::new();

    for ((backend_name, path_str), vars) in groups {
        let representative = vars[0];

        if !built.contains_key(backend_name) {
            let backend = resolve(backend_name).map_err(|cause| VarFetchError::Backend {
                var: representative.clone(),
                cause: Box::new(cause),
            })?;
            built.insert(backend_name, backend);
        }
        let backend = &built[backend_name];

        let path = SecretPath::parse(path_str).map_err(|cause| VarFetchError::Path {
            var: representative.clone(),
            cause,
        })?;

        let map = backend
            .get(&path)
            .await
            .map_err(|cause| VarFetchError::Read {
                var: representative.clone(),
                cause: Box::new(cause),
            })?
            .ok_or_else(|| VarFetchError::MissingPath {
                var: representative.clone(),
                command: representative.put_command(),
            })?;

        for var in vars {
            let key = SecretKey::parse(&var.key).map_err(|cause| VarFetchError::Key {
                var: var.clone(),
                cause,
            })?;

            // Key names are not secret; the values behind them are, and none
            // of them is named here.
            let value = map.get(&key).ok_or_else(|| VarFetchError::MissingKey {
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

    Ok(out)
}

#[cfg(test)]
mod tests {
    use secrecy::{ExposeSecret, SecretString};

    use super::*;
    use crate::secrets::fake::FakeBackend;
    use crate::secrets::SecretMap;

    fn var(backend: &str, path: &str, key: &str) -> SecretVar {
        SecretVar {
            backend: backend.to_string(),
            path: path.to_string(),
            key: key.to_string(),
        }
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
