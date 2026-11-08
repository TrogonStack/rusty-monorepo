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
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
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
#[path = "vars_tests.rs"]
mod tests;
