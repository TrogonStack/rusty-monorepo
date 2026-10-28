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

use secrecy::SecretString;
use tracing::field::Empty;
use tracing::Instrument;

use crate::config::{FetchedSecrets, SecretRead, SecretVar};
use crate::telemetry::semconv::trg::{
    SECRETS_ANSWERED_BY, SECRETS_BACKEND_KIND, SECRETS_BACKEND_NAME, SECRETS_FALLBACK_USED, SECRETS_VAR_COUNT,
};

use super::onepassword::{OnePasswordItem, OnePasswordReference, OpRead};
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

    /// A query reference is answered by `op read` alone, which names no
    /// fields the item does hold, so unlike [`VarFetchError::MissingKey`] there
    /// is nothing to list.
    #[error("var {var} found no such field on that item; check the field name in its reference")]
    MissingField { var: SecretVar },

    #[error("var {var} found no such key there (that entry holds: {present})")]
    MissingKey { var: SecretVar, present: String },

    /// Every address a read opted in to was tried, and each could not answer.
    /// Each attempt is the error it would have been on its own, so the list
    /// names the backend, the address and the reason for all of them.
    #[error("secret {read} could not be read from any of its addresses:{}", render_attempts(.attempts))]
    Exhausted {
        read: Box<SecretRead>,
        attempts: Vec<String>,
    },
}

fn render_attempts(attempts: &[String]) -> String {
    attempts.iter().map(|a| format!("\n  - {a}")).collect()
}

impl VarFetchError {
    /// A low-cardinality name for this failure, for `error.type`.
    pub fn error_type(&self) -> &'static str {
        match self {
            Self::Backend { .. } => "backend_config",
            Self::Read { cause, .. } => cause.error_type(),
            Self::MissingPath { .. } => "missing_path",
            Self::MissingItem { .. } => "missing_item",
            Self::MissingKey { .. } => "missing_key",
            Self::MissingField { .. } => "missing_field",
            Self::Exhausted { .. } => "exhausted",
        }
    }

    /// Whether a read that opted in to fallbacks may go on to its next
    /// address after this. See [`SecretsError::allows_fallback`] for where the
    /// line is drawn and why.
    ///
    /// Not finding the value counts, because a list that names a second store
    /// is the declaration that the first may not hold it yet, as it does while
    /// values are still being copied across. Exhaustion does not, since it is
    /// already the end of a list.
    pub fn allows_fallback(&self) -> bool {
        match self {
            Self::Backend { cause, .. } => cause.allows_fallback(),
            Self::Read { cause, .. } => cause.allows_fallback(),
            Self::MissingPath { .. }
            | Self::MissingItem { .. }
            | Self::MissingField { .. }
            | Self::MissingKey { .. } => true,
            Self::Exhausted { .. } => false,
        }
    }
}

/// Read every secret the given vars name, one round trip per distinct entry.
pub async fn fetch(registry: &Registry, wanted: &[SecretRead]) -> Result<FetchedSecrets, VarFetchError> {
    fetch_with(|name| registry.resolve(name), wanted).await
}

/// The span one round trip runs under. Names the backend and how many vars
/// the answer serves, and nothing about the entry it addresses.
fn round_trip_span(backend_name: &str, vars: usize) -> tracing::Span {
    tracing::info_span!(
        "fetch secret",
        { SECRETS_BACKEND_NAME } = backend_name,
        { SECRETS_BACKEND_KIND } = Empty,
        { SECRETS_VAR_COUNT } = i64::try_from(vars).unwrap_or(i64::MAX),
        "error.type" = Empty,
        "otel.status_code" = Empty,
    )
}

/// Runs one round trip under `span`, recording which kind of backend served
/// it and how it failed.
async fn traced<T>(
    span: tracing::Span,
    work: impl std::future::Future<Output = Result<T, VarFetchError>>,
) -> Result<T, VarFetchError> {
    let result = work.instrument(span.clone()).await;
    if let Err(e) = &result {
        super::telemetry::record_error(&span, e.error_type());
    }
    result
}

/// The same, against any way of naming a backend, so the grouping can be held
/// to its round-trip count without a reachable instance.
pub(super) async fn fetch_with<F>(resolve: F, wanted: &[SecretRead]) -> Result<FetchedSecrets, VarFetchError>
where
    F: Fn(&str) -> Result<Backend, BackendError>,
{
    let span = tracing::info_span!(
        "fetch secrets",
        { SECRETS_VAR_COUNT } = i64::try_from(wanted.len()).unwrap_or(i64::MAX),
        "error.type" = Empty,
        "otel.status_code" = Empty,
    );
    traced(span, fetch_grouped(resolve, wanted)).await
}

/// What the first address of a read came to.
///
/// A failure is kept as the text of the error rather than the error, because
/// one failed round trip can decline several reads at once and the error is
/// not something that can be handed to each of them.
enum Primary {
    Answered(SecretString),
    Declined(String),
}

async fn fetch_grouped<F>(resolve: F, wanted: &[SecretRead]) -> Result<FetchedSecrets, VarFetchError>
where
    F: Fn(&str) -> Result<Backend, BackendError>,
{
    let mut out = FetchedSecrets::new();
    if wanted.is_empty() {
        return Ok(out);
    }

    // A backend is built once per process even when several entries address it.
    let mut built: HashMap<String, Backend> = HashMap::new();

    for (read, primary) in read_primaries(&resolve, &mut built, wanted).await? {
        let value = if read.has_fallbacks() {
            let (value, answered_by) = resolve_chain(&resolve, &mut built, read, primary).await?;
            out.record_answered_by(read.clone(), answered_by);
            value
        } else {
            match primary {
                Primary::Answered(value) => value,
                Primary::Declined(_) => unreachable!("only a read with fallbacks is declined"),
            }
        };
        out.insert(read.clone(), value);
    }

    Ok(out)
}

/// Read the first address of every wanted read, one round trip per distinct
/// entry.
///
/// A round trip that fails fails the whole fetch, except where every read it
/// serves declared a fallback and the failure is one a fallback may answer.
/// Then those reads are declined rather than failed, and the span still
/// records what happened to the attempt.
async fn read_primaries<'a, F>(
    resolve: &F,
    built: &mut HashMap<String, Backend>,
    wanted: &'a [SecretRead],
) -> Result<Vec<(&'a SecretRead, Primary)>, VarFetchError>
where
    F: Fn(&str) -> Result<Backend, BackendError>,
{
    // Sorted rather than hashed, so a config with two unreachable backends
    // fails on the same one every run. Split by what a read answers with
    // rather than merged behind one key, so that selecting a value out of an
    // answer never has to consider a shape the group cannot hold.
    let mut at_path: BTreeMap<(&str, &SecretPath), Vec<(&SecretRead, &SecretKey)>> = BTreeMap::new();
    let mut in_item: BTreeMap<(&str, &OnePasswordItem), Vec<(&SecretRead, &OnePasswordReference)>> = BTreeMap::new();
    let mut derived: Vec<(&str, &SecretRead, &OnePasswordReference)> = Vec::new();
    for read in wanted {
        let var = read.primary();
        match var.address() {
            SecretAddress::OnePassword(r) if r.query().is_some() => derived.push((var.backend(), read, r.as_ref())),
            SecretAddress::Keychain(r) => at_path
                .entry((var.backend(), r.path()))
                .or_default()
                .push((read, r.key())),
            SecretAddress::Openbao(r) => at_path
                .entry((var.backend(), r.path()))
                .or_default()
                .push((read, r.key())),
            SecretAddress::OnePassword(r) => in_item
                .entry((var.backend(), r.item()))
                .or_default()
                .push((read, r.as_ref())),
        }
    }

    let mut out = Vec::with_capacity(wanted.len());

    for ((backend_name, path), vars) in at_path {
        let representative = vars[0].0.primary();
        let span = round_trip_span(backend_name, vars.len());
        // Selecting each var's value out of the map runs inside the traced
        // work, so a key missing from the answer fails the round-trip span
        // rather than only the parent fetch span.
        let selected = traced(span.clone(), async {
            let answer = async {
                let backend = build(resolve, built, backend_name, representative)?;
                span.record(SECRETS_BACKEND_KIND, backend.kind());
                backend
                    .get(path)
                    .await
                    .map_err(|cause| read_error(representative, cause))?
                    .ok_or_else(|| missing_entry(representative))
            }
            .await;
            let map = match answer {
                Ok(map) => map,
                Err(e) => return declined(&span, vars.iter().map(|&(read, _)| read), e),
            };

            let mut selected = Vec::with_capacity(vars.len());
            for &(read, key) in &vars {
                // Key names are not secret; the values behind them are, and
                // none of them is named here.
                let found = map.get(key).cloned().ok_or_else(|| VarFetchError::MissingKey {
                    var: read.primary().clone(),
                    present: map
                        .sorted_keys()
                        .iter()
                        .map(|k| k.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                });
                selected.push((read, settle(&span, read, found)?));
            }
            Ok(selected)
        })
        .await?;

        out.extend(selected);
    }

    for ((backend_name, item), vars) in in_item {
        let representative = vars[0].0.primary();
        let span = round_trip_span(backend_name, vars.len());
        let selected = traced(span.clone(), async {
            let answer = async {
                let backend = build(resolve, built, backend_name, representative)?;
                span.record(SECRETS_BACKEND_KIND, backend.kind());
                backend
                    .get_item(item)
                    .await
                    .map_err(|cause| read_error(representative, cause))?
                    .ok_or_else(|| missing_entry(representative))
            }
            .await;
            let fields = match answer {
                Ok(fields) => fields,
                Err(e) => return declined(&span, vars.iter().map(|&(read, _)| read), e),
            };

            let mut selected = Vec::with_capacity(vars.len());
            for &(read, reference) in &vars {
                let var = read.primary();
                let found = fields
                    .get(reference)
                    .map_err(|cause| read_error(var, cause))
                    .and_then(|found| {
                        found.cloned().ok_or_else(|| VarFetchError::MissingKey {
                            var: var.clone(),
                            present: fields.addresses().join(", "),
                        })
                    });
                selected.push((read, settle(&span, read, found)?));
            }
            Ok(selected)
        })
        .await?;

        out.extend(selected);
    }

    for (backend_name, read, reference) in derived {
        let var = read.primary();
        let span = round_trip_span(backend_name, 1);
        let found = traced(span.clone(), async {
            let found = async {
                let backend = build(resolve, built, backend_name, var)?;
                span.record(SECRETS_BACKEND_KIND, backend.kind());
                match backend.read(reference).await.map_err(|cause| read_error(var, cause))? {
                    OpRead::Value(value) => Ok(value),
                    OpRead::MissingItem => Err(missing_entry(var)),
                    OpRead::MissingField => Err(VarFetchError::MissingField { var: var.clone() }),
                }
            }
            .await;
            settle(&span, read, found)
        })
        .await?;
        out.push((read, found));
    }

    Ok(out)
}

/// Every read a failed round trip served, declined, when all of them opted in
/// to a fallback and the failure is one a fallback may answer. Otherwise the
/// failure stands, which is what a read with no fallback has always done.
fn declined<'a>(
    span: &tracing::Span,
    reads: impl Iterator<Item = &'a SecretRead>,
    error: VarFetchError,
) -> Result<Vec<(&'a SecretRead, Primary)>, VarFetchError> {
    let reads: Vec<&SecretRead> = reads.collect();
    if !error.allows_fallback() || !reads.iter().all(|read| read.has_fallbacks()) {
        return Err(error);
    }
    super::telemetry::record_error(span, error.error_type());
    let reason = error.to_string();
    Ok(reads
        .into_iter()
        .map(|read| (read, Primary::Declined(reason.clone())))
        .collect())
}

/// One read's own outcome, once its entry has been read: an answer, a decline
/// where it may fall back, or a failure.
fn settle(
    span: &tracing::Span,
    read: &SecretRead,
    found: Result<SecretString, VarFetchError>,
) -> Result<Primary, VarFetchError> {
    match found {
        Ok(value) => Ok(Primary::Answered(value)),
        Err(e) if e.allows_fallback() && read.has_fallbacks() => {
            super::telemetry::record_error(span, e.error_type());
            Ok(Primary::Declined(e.to_string()))
        }
        Err(e) => Err(e),
    }
}

/// Settle a read that declared fallbacks: the value, and a record of which
/// backend gave it.
///
/// The record is made whichever address answered, so a trace shows the first
/// one answering as plainly as it shows a later one doing so.
async fn resolve_chain<F>(
    resolve: &F,
    built: &mut HashMap<String, Backend>,
    read: &SecretRead,
    primary: Primary,
) -> Result<(SecretString, String), VarFetchError>
where
    F: Fn(&str) -> Result<Backend, BackendError>,
{
    let span = tracing::info_span!(
        "resolve secret",
        { SECRETS_ANSWERED_BY } = Empty,
        { SECRETS_FALLBACK_USED } = Empty,
        "error.type" = Empty,
        "otel.status_code" = Empty,
    );
    traced(span.clone(), async {
        let reason = match primary {
            Primary::Answered(value) => {
                span.record(SECRETS_ANSWERED_BY, read.primary().backend());
                span.record(SECRETS_FALLBACK_USED, false);
                return Ok((value, read.primary().backend().to_string()));
            }
            Primary::Declined(reason) => reason,
        };

        let mut attempts = vec![reason];
        for fallback in read.fallbacks() {
            let single = [SecretRead::from(fallback.clone())];
            match read_primaries(resolve, built, &single).await {
                Ok(mut answered) => {
                    let Some((_, Primary::Answered(value))) = answered.pop() else {
                        unreachable!("a read with no fallbacks is answered or it fails")
                    };
                    span.record(SECRETS_ANSWERED_BY, fallback.backend());
                    span.record(SECRETS_FALLBACK_USED, true);
                    eprintln!("{}", fallback_notice(read, fallback, &attempts));
                    return Ok((value, fallback.backend().to_string()));
                }
                Err(e) if e.allows_fallback() => attempts.push(e.to_string()),
                Err(e) => return Err(e),
            }
        }

        Err(VarFetchError::Exhausted {
            read: Box::new(read.clone()),
            attempts,
        })
    })
    .await
}

/// What goes to stderr when a later address answered, so a store that has
/// quietly stopped answering is something a person sees rather than something
/// they work out from a diverging value.
///
/// Names backends and the reasons they gave, which are the same words an
/// error would print, and never a value.
pub(crate) fn fallback_notice(read: &SecretRead, answered: &SecretVar, attempts: &[String]) -> String {
    let mut out = format!(
        "warning: backend `{}` could not answer, so the value was read from backend `{}`",
        read.primary().backend(),
        answered.backend()
    );
    for attempt in attempts {
        out.push_str("\n  - ");
        out.push_str(attempt);
    }
    out
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
    use crate::config::VarResolveError;
    use crate::secrets::fake::{FakeBackend, FakeFailure};
    use crate::secrets::openbao::OpenbaoReference;
    use crate::secrets::SecretMap;

    fn address(backend: &str, path: &str, key: &str) -> SecretVar {
        SecretVar::new(
            backend.to_string(),
            SecretAddress::Openbao(OpenbaoReference::new(
                SecretPath::parse(path).expect("path"),
                SecretKey::parse(key).expect("key"),
            )),
        )
    }

    fn var(backend: &str, path: &str, key: &str) -> SecretRead {
        SecretRead::single(address(backend, path, key))
    }

    /// A var addressed the way 1Password is addressed, over the same stand-in
    /// backend: the fake answers an item read out of the entry at
    /// `"<vault>/<item>"`, so the round-trip count is comparable.
    fn op_address(backend: &str, reference: &str) -> SecretVar {
        SecretVar::new(
            backend.to_string(),
            SecretAddress::OnePassword(Box::new(OnePasswordReference::parse(reference).expect("reference"))),
        )
    }

    fn op_var(backend: &str, reference: &str) -> SecretRead {
        SecretRead::single(op_address(backend, reference))
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

    #[tokio::test]
    async fn a_derived_reference_is_read_by_itself_beside_the_plain_ones() {
        let fake = seeded(&[("Ops/deploy", &[("TOKEN", "t"), ("private key", "k")])]).await;
        let backend = Backend::Fake(fake.clone());

        let wanted = vec![
            op_var("op", "op://Ops/deploy/TOKEN"),
            op_var("op", "op://Ops/deploy/private key?ssh-format=openssh"),
        ];
        let fetched = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap();

        assert_eq!(fetched.get(&wanted[0]).unwrap().expose_secret(), "t");
        assert_eq!(fetched.get(&wanted[1]).unwrap().expose_secret(), "k");
        assert_eq!(fake.get_count(), 2, "one item read and one derived read");
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

    #[test]
    fn each_round_trip_is_a_span_under_the_fetch_naming_no_value() {
        use crate::secrets::telemetry::testing::{assert_never_recorded, current_thread};

        const SECRET: &str = "vars-span-secret-4d1e";
        let (fetched, trace) = crate::telemetry::testing::capture(|| {
            current_thread(async {
                let fake = seeded(&[
                    ("deploy-keys", &[("token", SECRET), ("principal", "p")]),
                    ("other", &[("k", "v")]),
                ])
                .await;
                let backend = Backend::Fake(fake);
                let wanted = vec![
                    var("homelab", "deploy-keys", "token"),
                    var("homelab", "deploy-keys", "principal"),
                    var("homelab", "other", "k"),
                ];
                fetch_with(|_| Ok(backend.clone()), &wanted).await
            })
        });
        assert_eq!(fetched.expect("fetch").len(), 3);

        let parent = trace.span("fetch secrets").expect("the fetch span");
        assert_eq!(
            trace.attribute("fetch secrets", "trg.secrets.var.count"),
            Some(opentelemetry::Value::I64(3))
        );
        let round_trips = trace.children_of(parent);
        assert_eq!(round_trips.len(), 2, "one span per distinct entry");
        let mut counts = Vec::new();
        for span in round_trips {
            assert_eq!(span.name, "fetch secret");
            let attribute = |key: &str| {
                span.attributes
                    .iter()
                    .find(|kv| kv.key.as_str() == key)
                    .map(|kv| kv.value.to_string())
            };
            assert_eq!(attribute("trg.secrets.backend.kind").as_deref(), Some("fake"));
            assert_eq!(attribute("trg.secrets.backend.name").as_deref(), Some("homelab"));
            counts.push(attribute("trg.secrets.var.count").expect("count"));
        }
        counts.sort();
        assert_eq!(counts, ["1", "2"]);

        assert_never_recorded(&trace, &[SECRET, "deploy-keys", "principal"]);
    }

    #[test]
    fn a_missing_entry_fails_its_round_trip_and_the_fetch() {
        use crate::secrets::telemetry::testing::current_thread;
        use opentelemetry::trace::Status;

        let (fetched, trace) = crate::telemetry::testing::capture(|| {
            current_thread(async {
                let backend = Backend::Fake(FakeBackend::new());
                fetch_with(|_| Ok(backend.clone()), &[var("homelab", "absent", "k")]).await
            })
        });
        assert!(fetched.is_err());

        for name in ["fetch secrets", "fetch secret"] {
            let span = trace.span(name).expect(name);
            assert!(matches!(span.status, Status::Error { .. }), "{name}: {:?}", span.status);
            assert_eq!(
                trace.attribute(name, "error.type").map(|v| v.to_string()).as_deref(),
                Some("missing_path")
            );
        }
    }

    /// The entry round trip succeeds; it is picking the wanted key out of it
    /// that fails. That selection runs inside the traced work, so the
    /// round-trip span must carry the failure too, not only the parent fetch.
    #[test]
    fn a_missing_key_fails_its_round_trip_and_the_fetch() {
        use crate::secrets::telemetry::testing::current_thread;
        use opentelemetry::trace::Status;

        let (fetched, trace) = crate::telemetry::testing::capture(|| {
            current_thread(async {
                let fake = seeded(&[("agentgateway", &[("token", "t")])]).await;
                let backend = Backend::Fake(fake);
                fetch_with(|_| Ok(backend.clone()), &[var("homelab", "agentgateway", "nope")]).await
            })
        });
        assert!(fetched.is_err());

        for name in ["fetch secrets", "fetch secret"] {
            let span = trace.span(name).expect(name);
            assert!(matches!(span.status, Status::Error { .. }), "{name}: {:?}", span.status);
            assert_eq!(
                trace.attribute(name, "error.type").map(|v| v.to_string()).as_deref(),
                Some("missing_key")
            );
        }
    }

    /// The same, for a 1Password reference whose field is not on the item:
    /// the lookup happens after `get_item` succeeds, so it must still be
    /// attributed to that round trip.
    #[test]
    fn a_missing_field_in_an_item_fails_its_round_trip_and_the_fetch() {
        use crate::secrets::telemetry::testing::current_thread;
        use opentelemetry::trace::Status;

        let (fetched, trace) = crate::telemetry::testing::capture(|| {
            current_thread(async {
                let fake = seeded(&[("Ops/deploy", &[("TOKEN", "t")])]).await;
                let backend = Backend::Fake(fake);
                fetch_with(|_| Ok(backend.clone()), &[op_var("op", "op://Ops/deploy/NOPE")]).await
            })
        });
        assert!(fetched.is_err());

        for name in ["fetch secrets", "fetch secret"] {
            let span = trace.span(name).expect(name);
            assert!(matches!(span.status, Status::Error { .. }), "{name}: {:?}", span.status);
            assert_eq!(
                trace.attribute(name, "error.type").map(|v| v.to_string()).as_deref(),
                Some("missing_key")
            );
        }
    }

    /// A mistyped field on an item that exists must not be reported as an item
    /// to create.
    #[tokio::test]
    async fn a_missing_field_on_a_derived_read_is_not_reported_as_a_missing_item() {
        let fake = seeded(&[("Ops/deploy", &[("TOKEN", "t")])]).await;
        let backend = Backend::Fake(fake);
        let wanted = vec![op_var("op", "op://Ops/deploy/nope?ssh-format=openssh")];

        let err = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap_err();

        assert!(matches!(err, VarFetchError::MissingField { .. }), "{err}");
        assert_eq!(err.error_type(), "missing_field");
        let msg = err.to_string();
        assert!(msg.contains("no such field"), "{msg}");
        assert!(!msg.contains("create it"), "{msg}");
    }

    #[tokio::test]
    async fn a_missing_item_on_a_derived_read_keeps_the_create_it_message() {
        let fake = seeded(&[]).await;
        let backend = Backend::Fake(fake);
        let wanted = vec![op_var("op", "op://Ops/deploy/key?ssh-format=openssh")];

        let err = fetch_with(|_| Ok(backend.clone()), &wanted).await.unwrap_err();

        assert!(matches!(err, VarFetchError::MissingItem { .. }), "{err}");
        assert!(err.to_string().contains("create it in 1Password first"), "{err}");
    }

    /// Two stores standing in for the two kinds of backend a fallback list
    /// crosses: OpenBao-shaped addressing first, a 1Password reference second.
    struct Stores {
        bao: FakeBackend,
        op: FakeBackend,
    }

    impl Stores {
        async fn new(bao: &[(&str, &[(&str, &str)])], op: &[(&str, &[(&str, &str)])]) -> Self {
            Self {
                bao: seeded(bao).await,
                op: seeded(op).await,
            }
        }

        fn resolve(&self, name: &str) -> Result<Backend, BackendError> {
            match name {
                "bao" => Ok(Backend::Fake(self.bao.clone())),
                "op" => Ok(Backend::Fake(self.op.clone())),
                other => Err(BackendError::Unknown {
                    name: other.to_string(),
                    declared: "bao, op".to_string(),
                }),
            }
        }
    }

    fn bao_then_op() -> SecretRead {
        SecretRead::from_addresses(vec![
            address("bao", "exec/github", "GH_TOKEN"),
            op_address("op", "op://Ops/github/GH_TOKEN"),
        ])
        .expect("two addresses")
    }

    async fn fetch_chain(stores: &Stores, read: &SecretRead) -> Result<FetchedSecrets, VarFetchError> {
        fetch_with(|name| stores.resolve(name), std::slice::from_ref(read)).await
    }

    #[tokio::test]
    async fn the_first_address_answering_leaves_the_rest_unread() {
        let stores = Stores::new(
            &[("exec/github", &[("GH_TOKEN", "from-bao")])],
            &[("Ops/github", &[("GH_TOKEN", "from-op")])],
        )
        .await;
        let read = bao_then_op();

        let fetched = fetch_chain(&stores, &read).await.unwrap();

        assert_eq!(fetched.get(&read).unwrap().expose_secret(), "from-bao");
        assert_eq!(fetched.answered_by(&read), Some("bao"));
        assert_eq!(
            stores.op.get_count(),
            0,
            "a fallback is not consulted while the first answers"
        );
    }

    #[tokio::test]
    async fn an_unreachable_first_backend_falls_back_to_the_next() {
        let stores = Stores::new(&[], &[("Ops/github", &[("GH_TOKEN", "from-op")])]).await;
        stores.bao.set_get_failure(Some(FakeFailure::Transport));
        let read = bao_then_op();

        let fetched = fetch_chain(&stores, &read).await.unwrap();

        assert_eq!(fetched.get(&read).unwrap().expose_secret(), "from-op");
        assert_eq!(fetched.answered_by(&read), Some("op"));
    }

    /// A reference with a query is derived by `op` rather than listed on the
    /// item, so it is read by its own call, and as a fallback it must still
    /// answer after the first address cannot.
    #[tokio::test]
    async fn a_derived_reference_answers_as_a_fallback() {
        let stores = Stores::new(&[], &[("Ops/deploy-key", &[("private key", "key-material")])]).await;
        stores.bao.set_get_failure(Some(FakeFailure::Transport));
        let read = SecretRead::from_addresses(vec![
            address("bao", "exec/ssh", "KEY"),
            op_address("op", "op://Ops/deploy-key/private key?ssh-format=openssh"),
        ])
        .unwrap();

        let fetched = fetch_chain(&stores, &read).await.unwrap();

        assert_eq!(fetched.get(&read).unwrap().expose_secret(), "key-material");
        assert_eq!(fetched.answered_by(&read), Some("op"));
    }

    /// A field one item lacks may be on another, so a field miss on a derived
    /// reference is a miss like any other and moves on.
    #[tokio::test]
    async fn a_derived_reference_whose_field_is_missing_falls_back() {
        let stores = Stores::new(
            &[],
            &[
                ("Ops/deploy-key", &[("other", "x")]),
                ("Ops/spare-key", &[("private key", "key-material")]),
            ],
        )
        .await;
        let read = SecretRead::from_addresses(vec![
            op_address("op", "op://Ops/deploy-key/private key?ssh-format=openssh"),
            op_address("op", "op://Ops/spare-key/private key?ssh-format=openssh"),
        ])
        .unwrap();

        let fetched = fetch_chain(&stores, &read).await.unwrap();

        assert_eq!(fetched.get(&read).unwrap().expose_secret(), "key-material");
    }

    #[tokio::test]
    async fn the_exhausted_error_names_a_field_miss() {
        let stores = Stores::new(&[], &[("Ops/deploy-key", &[("other", "x")])]).await;
        let read = SecretRead::from_addresses(vec![
            op_address("op", "op://Ops/deploy-key/private key?ssh-format=openssh"),
            address("bao", "exec/ssh", "KEY"),
        ])
        .unwrap();

        let err = fetch_chain(&stores, &read).await.unwrap_err();

        let msg = err.to_string();
        assert!(matches!(err, VarFetchError::Exhausted { .. }), "{msg}");
        assert!(msg.contains("no such field"), "{msg}");
        assert!(msg.contains("found nothing at that path"), "{msg}");
    }

    /// A derived reference as the first address is declined like any other
    /// when it cannot answer.
    #[tokio::test]
    async fn a_derived_reference_that_cannot_answer_first_falls_back() {
        let stores = Stores::new(&[("exec/ssh", &[("KEY", "from-bao")])], &[]).await;
        let read = SecretRead::from_addresses(vec![
            op_address("op", "op://Ops/deploy-key/private key?ssh-format=openssh"),
            address("bao", "exec/ssh", "KEY"),
        ])
        .unwrap();

        let fetched = fetch_chain(&stores, &read).await.unwrap();

        assert_eq!(fetched.get(&read).unwrap().expose_secret(), "from-bao");
    }

    /// A backend that cannot be built on this machine, as a backend whose
    /// address comes from an unset environment variable cannot, is the case a
    /// fallback exists for.
    #[tokio::test]
    async fn a_first_backend_that_cannot_be_built_here_falls_back() {
        let stores = Stores::new(&[], &[("Ops/github", &[("GH_TOKEN", "from-op")])]).await;
        let read = bao_then_op();

        let fetched = fetch_with(
            |name| match name {
                "bao" => Err(BackendError::Resolve {
                    name: "bao".to_string(),
                    field: "addr",
                    cause: Box::new(VarResolveError::MissingEnv("BAO_ADDR".to_string())),
                }),
                other => stores.resolve(other),
            },
            std::slice::from_ref(&read),
        )
        .await
        .unwrap();

        assert_eq!(fetched.get(&read).unwrap().expose_secret(), "from-op");
    }

    /// A value that is not there yet is what the list is for while values are
    /// being copied across, so it falls through rather than failing.
    #[tokio::test]
    async fn a_value_the_first_backend_does_not_hold_falls_back() {
        let read = bao_then_op();

        let no_entry = Stores::new(&[], &[("Ops/github", &[("GH_TOKEN", "from-op")])]).await;
        let fetched = fetch_chain(&no_entry, &read).await.unwrap();
        assert_eq!(fetched.get(&read).unwrap().expose_secret(), "from-op");

        let no_key = Stores::new(
            &[("exec/github", &[("OTHER", "x")])],
            &[("Ops/github", &[("GH_TOKEN", "from-op")])],
        )
        .await;
        let fetched = fetch_chain(&no_key, &read).await.unwrap();
        assert_eq!(fetched.get(&read).unwrap().expose_secret(), "from-op");
    }

    /// An entry that is there but will not decode is a store holding something
    /// wrong, and the next address answering would paper over it.
    #[tokio::test]
    async fn a_first_backend_holding_a_malformed_entry_stops_the_read() {
        let stores = Stores::new(&[], &[("Ops/github", &[("GH_TOKEN", "from-op")])]).await;
        stores.bao.set_get_failure(Some(FakeFailure::Malformed));
        let read = bao_then_op();

        let err = fetch_chain(&stores, &read).await.unwrap_err();

        assert!(matches!(err, VarFetchError::Read { .. }), "{err}");
        assert_eq!(stores.op.get_count(), 0);
    }

    /// A configuration mistake is wrong on every machine, so a fallback that
    /// answered would hide it for exactly as long as the fallback kept working.
    #[tokio::test]
    async fn a_first_backend_that_is_not_declared_stops_the_read() {
        let stores = Stores::new(&[], &[("Ops/github", &[("GH_TOKEN", "from-op")])]).await;
        let read = SecretRead::from_addresses(vec![
            address("typo", "exec/github", "GH_TOKEN"),
            op_address("op", "op://Ops/github/GH_TOKEN"),
        ])
        .unwrap();

        let err = fetch_chain(&stores, &read).await.unwrap_err();

        assert!(matches!(err, VarFetchError::Backend { .. }), "{err}");
        assert_eq!(stores.op.get_count(), 0);
    }

    #[tokio::test]
    async fn a_malformed_fallback_stops_the_read_rather_than_being_skipped() {
        let stores = Stores::new(&[], &[]).await;
        stores.op.set_get_failure(Some(FakeFailure::Malformed));
        let read = SecretRead::from_addresses(vec![
            address("bao", "exec/github", "GH_TOKEN"),
            op_address("op", "op://Ops/github/GH_TOKEN"),
            address("bao", "exec/other", "GH_TOKEN"),
        ])
        .unwrap();

        let err = fetch_chain(&stores, &read).await.unwrap_err();

        assert!(matches!(err, VarFetchError::Read { .. }), "{err}");
    }

    #[tokio::test]
    async fn when_every_address_fails_the_error_lists_each_one_and_why() {
        let stores = Stores::new(&[], &[]).await;
        stores.bao.set_get_failure(Some(FakeFailure::Transport));
        let read = bao_then_op();

        let err = fetch_chain(&stores, &read).await.unwrap_err();

        let msg = err.to_string();
        assert!(matches!(err, VarFetchError::Exhausted { .. }), "{msg}");
        assert!(msg.contains("`GH_TOKEN` at `exec/github` from backend `bao`"), "{msg}");
        assert!(msg.contains("transport: injected"), "{msg}");
        assert!(msg.contains("`op://Ops/github/GH_TOKEN` from backend `op`"), "{msg}");
        assert!(msg.contains("found no such item"), "{msg}");
    }

    /// Declining is per read. One that did not opt in to a fallback still
    /// fails the fetch, and nothing is read on behalf of the other.
    #[tokio::test]
    async fn a_read_sharing_an_entry_with_one_that_has_no_fallback_still_fails() {
        let stores = Stores::new(&[], &[("Ops/github", &[("GH_TOKEN", "from-op")])]).await;
        let covered = bao_then_op();
        let bare = var("bao", "exec/github", "OTHER");

        let err = fetch_with(|name| stores.resolve(name), &[covered, bare])
            .await
            .unwrap_err();

        assert!(matches!(err, VarFetchError::MissingPath { .. }), "{err}");
    }

    #[tokio::test]
    async fn reads_that_fall_back_to_the_same_entry_each_get_their_own_value() {
        let stores = Stores::new(&[], &[("Ops/github", &[("GH_TOKEN", "t"), ("GH_USER", "u")])]).await;
        let token = SecretRead::from_addresses(vec![
            address("bao", "exec/github", "GH_TOKEN"),
            op_address("op", "op://Ops/github/GH_TOKEN"),
        ])
        .unwrap();
        let user = SecretRead::from_addresses(vec![
            address("bao", "exec/github", "GH_USER"),
            op_address("op", "op://Ops/github/GH_USER"),
        ])
        .unwrap();

        let fetched = fetch_with(|name| stores.resolve(name), &[token.clone(), user.clone()])
            .await
            .unwrap();

        assert_eq!(fetched.get(&token).unwrap().expose_secret(), "t");
        assert_eq!(fetched.get(&user).unwrap().expose_secret(), "u");
        assert_eq!(stores.bao.get_count(), 1, "the first addresses still share one read");
    }

    #[test]
    fn the_notice_names_the_backends_and_the_reasons_and_no_value() {
        let read = bao_then_op();
        let notice = fallback_notice(
            &read,
            &read.fallbacks()[0],
            &["var `GH_TOKEN` at `exec/github` from backend `bao` could not be read: transport: down".to_string()],
        );

        assert!(
            notice.starts_with("warning: backend `bao` could not answer"),
            "{notice}"
        );
        assert!(notice.contains("read from backend `op`"), "{notice}");
        assert!(notice.contains("transport: down"), "{notice}");
        assert!(!notice.contains("from-op"), "{notice}");
    }

    /// Which backend answered is recorded whether or not a fallback was
    /// needed, so a trace reads the same on a healthy machine as on one that
    /// has drifted.
    #[test]
    fn the_backend_that_answered_is_recorded_on_a_span() {
        use crate::secrets::telemetry::testing::{assert_never_recorded, current_thread};

        const SECRET: &str = "chain-span-secret-7b2f";
        let (answers, trace) = crate::telemetry::testing::capture(|| {
            current_thread(async {
                let stores = Stores::new(
                    &[("exec/github", &[("GH_TOKEN", SECRET)])],
                    &[("Ops/github", &[("GH_TOKEN", SECRET)])],
                )
                .await;
                let read = bao_then_op();
                let healthy = fetch_chain(&stores, &read).await.is_ok();
                stores.bao.set_get_failure(Some(FakeFailure::Transport));
                let drifted = fetch_chain(&stores, &read).await.is_ok();
                (healthy, drifted)
            })
        });
        assert_eq!(answers, (true, true));

        let answered_by: Vec<(String, String)> = trace
            .spans_named("resolve secret")
            .iter()
            .map(|span| {
                let attribute = |key: &str| {
                    span.attributes
                        .iter()
                        .find(|kv| kv.key.as_str() == key)
                        .map(|kv| kv.value.to_string())
                        .expect(key)
                };
                (
                    attribute("trg.secrets.answered_by"),
                    attribute("trg.secrets.fallback.used"),
                )
            })
            .collect();
        assert_eq!(
            answered_by,
            [
                ("bao".to_string(), "false".to_string()),
                ("op".to_string(), "true".to_string())
            ]
        );

        assert_never_recorded(&trace, &[SECRET, "exec/github", "GH_TOKEN"]);
    }
}
