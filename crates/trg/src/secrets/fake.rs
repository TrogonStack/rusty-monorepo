use super::*;
use std::sync::{Arc, Mutex};

/// Read failures a test can inject, so that callers which must distinguish
/// "unreadable" from "unreachable" can be exercised.
#[derive(Clone, Debug)]
pub enum FakeFailure {
    Transport,
    Malformed,
    /// Like `Malformed`, but as if the backend had the exact bytes that
    /// failed to decode handy, for exercising a caller's fallback onto an
    /// older shape.
    MalformedWithRaw(String),
}

/// What a [`FakeFailure`] turns into on the wire, for the path that hit
/// it. Shared by the blanket failure and the per-path one below it, so
/// the two injection points cannot drift into answering differently.
fn fake_failure_error(path: &SecretPath, failure: &FakeFailure) -> SecretsError {
    match failure {
        FakeFailure::Transport => SecretsError::Transport("injected".to_string()),
        FakeFailure::Malformed => SecretsError::Malformed {
            path: path.clone(),
            cause: "injected".to_string(),
            raw: None,
        },
        FakeFailure::MalformedWithRaw(raw) => SecretsError::Malformed {
            path: path.clone(),
            cause: "injected".to_string(),
            raw: Some(SecretString::from(raw.clone())),
        },
    }
}

/// In-memory backend for unit tests.
///
/// `#[cfg(test)]`, so it costs nothing in a release build. Integration
/// tests under `tests/` compile this crate without `cfg(test)` and cannot
/// see it; those drive the CLI against a real backend instead.
#[derive(Clone, Default)]
pub struct FakeBackend {
    entries: Arc<Mutex<HashMap<SecretPath, SecretMap>>>,
    get_failure: Arc<Mutex<Option<FakeFailure>>>,
    /// Failures scoped to one path, checked before the blanket one, for a
    /// caller that reads two paths in the same operation and has to tell
    /// "the other path errored" from "the other path just isn't there".
    get_failure_at: Arc<Mutex<HashMap<SecretPath, FakeFailure>>>,
    set_failure: Arc<Mutex<bool>>,
    discard_writes: Arc<Mutex<bool>>,
    gets: Arc<Mutex<usize>>,
}

impl FakeBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many reads have been issued, so a caller that claims to batch
    /// can be held to it.
    pub fn get_count(&self) -> usize {
        *self.gets.lock().expect("fake backend lock")
    }

    /// Make every subsequent `get` fail until cleared with `None`.
    pub fn set_get_failure(&self, failure: Option<FakeFailure>) {
        *self.get_failure.lock().expect("fake backend lock") = failure;
    }

    /// Make `get` fail at exactly `path`, leaving every other path
    /// (including the blanket failure set by [`Self::set_get_failure`])
    /// unaffected.
    pub fn set_get_failure_at(&self, path: SecretPath, failure: FakeFailure) {
        self.get_failure_at
            .lock()
            .expect("fake backend lock")
            .insert(path, failure);
    }

    /// Lift a failure set by [`Self::set_get_failure_at`], for a caller
    /// that has to exercise what happens *after* a transient read error,
    /// not just during one.
    pub fn clear_get_failure_at(&self, path: &SecretPath) {
        self.get_failure_at.lock().expect("fake backend lock").remove(path);
    }

    /// Make every subsequent `set` fail until cleared, for exercising a
    /// caller that must tolerate a write it issued along the way (such as
    /// a migration) failing without failing the call that triggered it.
    pub fn set_set_failure(&self, fail: bool) {
        *self.set_failure.lock().expect("fake backend lock") = fail;
    }

    /// Acknowledge every subsequent `set` without keeping it, for
    /// exercising a caller that must not trust an acknowledgement.
    pub fn set_discard_writes(&self, discard: bool) {
        *self.discard_writes.lock().expect("fake backend lock") = discard;
    }

    pub async fn get(&self, path: &SecretPath) -> Result<Option<SecretMap>, SecretsError> {
        *self.gets.lock().expect("fake backend lock") += 1;
        if let Some(failure) = self.get_failure_at.lock().expect("fake backend lock").get(path) {
            return Err(fake_failure_error(path, failure));
        }
        if let Some(failure) = &*self.get_failure.lock().expect("fake backend lock") {
            return Err(fake_failure_error(path, failure));
        }
        Ok(self.entries.lock().expect("fake backend lock").get(path).cloned())
    }

    /// Stand in for a 1Password item read, over the same
    /// `"<vault>/<title>"` entry `get` uses, so a caller that claims to
    /// spend one read per item can be held to it without an `op` binary.
    pub async fn get_item(&self, item: &OnePasswordItem) -> Result<Option<OnePasswordItemFields>, SecretsError> {
        let path = SecretPath::parse(&item.to_string()).map_err(|cause| SecretsError::MalformedItem {
            item: item.clone(),
            cause: cause.to_string(),
        })?;
        Ok(self
            .get(&path)
            .await?
            .map(|map| OnePasswordItemFields::from_secret_map(item, &map)))
    }

    /// Stand in for `op read`, answering the stored field whatever the
    /// query asks for.
    pub async fn read(&self, reference: &OnePasswordReference) -> Result<OpRead, SecretsError> {
        let Some(fields) = self.get_item(reference.item()).await? else {
            return Ok(OpRead::MissingItem);
        };
        Ok(fields
            .get(reference)?
            .cloned()
            .map_or(OpRead::MissingField, OpRead::Value))
    }

    pub async fn set(&self, path: &SecretPath, map: &SecretMap) -> Result<(), SecretsError> {
        if *self.set_failure.lock().expect("fake backend lock") {
            return Err(SecretsError::Transport("injected".to_string()));
        }
        if *self.discard_writes.lock().expect("fake backend lock") {
            return Ok(());
        }
        self.entries
            .lock()
            .expect("fake backend lock")
            .insert(path.clone(), map.clone());
        Ok(())
    }

    pub async fn delete(&self, path: &SecretPath) -> Result<(), SecretsError> {
        self.entries.lock().expect("fake backend lock").remove(path);
        Ok(())
    }

    pub async fn list(&self, prefix: Option<&SecretPath>) -> Result<Vec<String>, SecretsError> {
        let entries = self.entries.lock().expect("fake backend lock");
        let mut out: Vec<String> = entries
            .keys()
            .filter(|p| match prefix {
                Some(prefix) => p.as_str().starts_with(prefix.as_str()),
                None => true,
            })
            .map(|p| p.as_str().to_string())
            .collect();
        out.sort();
        Ok(out)
    }
}
