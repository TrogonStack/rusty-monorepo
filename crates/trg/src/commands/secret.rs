//! `trg secret get`, `trg secret put` and `trg secret mirror`.
//!
//! A secret has an identity of its own rather than one borrowed from whichever
//! MCP server happens to read it, so the arguments here are the inline table
//! from `[mcp.servers.<name>.vars]` spelled out on a command line: the same
//! backend name, addressed in the same vocabulary. A value that reads here
//! reads there, and a var that is refused here is refused at load too, because
//! both go through [`SecretVar::resolve_at`].

mod constants;
use constants::OTHER_ERROR;
use std::io::{IsTerminal, Read, Write};

use clap::{Args, Subcommand};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use tracing::field::Empty;
use tracing::Instrument;

use crate::config::{RawSecretVar, SecretRead, SecretVar, VarSite};
use crate::exit_code::ExitCode;
use crate::output::{print_json, OutputFormat};
use crate::secrets::mirror as secrets_mirror;
use crate::secrets::onepassword::{OnePasswordReference, OpRead};
use crate::secrets::telemetry::record_error;
use crate::secrets::{
    vars, Backend, BackendKind, Registry, SecretAddress, SecretKey, SecretMap, SecretName, SecretPath,
};
use crate::secrets::{Mirror, MirrorError, MirrorOutcome, TargetOutcomes};
use crate::telemetry::semconv::trg::SECRETS_MIRROR_NAME;
use crate::telemetry::semconv::trg::{SECRETS_BACKEND_KIND, SECRETS_BACKEND_NAME};

#[derive(Subcommand)]
pub enum SecretCommands {
    /// Read one value out of a secrets backend
    Get(GetArgs),
    /// Write one value into a secrets backend, reading it from stdin
    Put(PutArgs),
    /// Copy values from their source into the targets `[secrets.mirrors.<name>]` declares
    Mirror(MirrorArgs),
}

/// What every subcommand needs whatever it addresses.
#[derive(Args)]
pub struct CommonArgs {
    /// The `[secrets.backends.<name>]` entry to address
    #[arg(long)]
    pub backend: String,

    /// Output format. For `get`, whose result is the secret itself, `json`
    /// carries the value in the document exactly as `text` writes it to
    /// stdout; neither is the safer one to log.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub output_format: OutputFormat,
}

/// One value's address, in whichever vocabulary its backend speaks.
///
/// `clap` is what keeps the two vocabularies from being mixed: `--ref`
/// conflicts with both of the others and `--path` and `--key` require each
/// other, so a blended or half-written address is refused before any of this
/// runs. What clap cannot know is which vocabulary the named backend speaks,
/// since that is declared in the config rather than typed on the command line.
/// [`SecretVar::resolve_at`] settles that, which is the same call config load
/// makes, so the two surfaces cannot drift on what they accept.
#[derive(Args)]
pub struct AddressArgs {
    /// The entry within that backend, relative to its configured prefix
    #[arg(
        long,
        requires = "key",
        conflicts_with_all = ["reference", "name"],
        required_unless_present_any = ["reference", "name"]
    )]
    pub path: Option<String>,

    /// The field within that entry
    #[arg(long, requires = "path", conflicts_with_all = ["reference", "name"])]
    pub key: Option<String>,

    /// A 1Password secret reference, as `Copy Secret Reference` yields it
    #[arg(long = "ref", value_name = "op://...", conflicts_with = "name")]
    pub reference: Option<String>,
}

#[derive(Args)]
pub struct GetArgs {
    /// A secret declared under `[secrets.vars.<name>]`, read with the address
    /// and fallbacks it declares. Takes the place of every address flag
    #[arg(long, value_name = "NAME", value_parser = SecretName::parse)]
    pub name: Option<SecretName>,

    /// The `[secrets.backends.<name>]` entry to address
    #[arg(long, required_unless_present = "name", conflicts_with = "name")]
    pub backend: Option<String>,

    /// Output format. `json` carries the value in the document exactly as
    /// `text` writes it to stdout; neither is the safer one to log.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub output_format: OutputFormat,

    #[command(flatten)]
    pub address: AddressArgs,

    /// Another address to read when this one cannot answer, written as the
    /// inline table a config holds, for example `'{ backend = "op", ref =
    /// "op://<vault>/<item>/<field>" }'`. Repeat to add more, tried in order.
    /// Which backend answered is reported on stderr, never on stdout.
    #[arg(long = "fallback", value_name = "TABLE", value_parser = parse_fallback, conflicts_with = "name")]
    pub fallbacks: Vec<RawSecretVar>,
}

/// One `--fallback`, read the way a config would read the same table, so the
/// flag cannot accept an address the config would refuse.
fn parse_fallback(text: &str) -> Result<RawSecretVar, String> {
    #[derive(serde::Deserialize)]
    struct Fallback {
        fallback: RawSecretVar,
    }
    toml::from_str::<Fallback>(&format!("fallback = {text}"))
        .map(|parsed| parsed.fallback)
        .map_err(|e| e.message().to_string())
}

impl GetArgs {
    fn raw(&self) -> RawSecretVar {
        RawSecretVar {
            backend: self.backend.clone().unwrap_or_default(),
            path: self.address.path.clone(),
            key: self.address.key.clone(),
            reference: self.address.reference.clone(),
        }
    }
}

/// `put` takes no `--ref`, because the only backend addressed that way is
/// read-only: there is no reference this command could act on.
#[derive(Args)]
pub struct PutArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    /// The entry within that backend, relative to its configured prefix
    #[arg(long)]
    pub path: String,

    /// The field within that entry
    #[arg(long)]
    pub key: String,
}

impl PutArgs {
    fn raw(&self) -> RawSecretVar {
        RawSecretVar {
            backend: self.common.backend.clone(),
            path: Some(self.path.clone()),
            key: Some(self.key.clone()),
            reference: None,
        }
    }
}

/// A mirror writes only when asked to, because the targets it overwrites are
/// the ones other things already read from. Without `apply` nothing is written
/// and each target reports what it would do.
#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct MirrorArgs {
    #[command(subcommand)]
    pub action: Option<MirrorAction>,

    #[command(flatten)]
    pub selection: MirrorSelection,
}

#[derive(Subcommand)]
pub enum MirrorAction {
    /// Write the targets
    Apply(MirrorSelection),
}

#[derive(Args)]
pub struct MirrorSelection {
    /// A mirror to run, by the name it is declared under. Repeat to run more.
    /// All of them when none is given
    #[arg(long = "name", value_name = "NAME")]
    pub names: Vec<String>,

    /// Output format. Neither carries a value, only what each target did
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub output_format: OutputFormat,
}

impl MirrorArgs {
    fn apply(&self) -> bool {
        matches!(self.action, Some(MirrorAction::Apply(_)))
    }

    fn selection(&self) -> &MirrorSelection {
        match &self.action {
            Some(MirrorAction::Apply(selection)) => selection,
            None => &self.selection,
        }
    }
}

impl SecretCommands {
    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            debug_remnants,
            reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
        )
    )]
    pub async fn handle(self, registry: &Registry) -> ExitCode {
        let span = self.span();
        let result = match self {
            SecretCommands::Get(args) => get(registry, &args).instrument(span.clone()).await,
            SecretCommands::Put(args) => put(registry, &args).instrument(span.clone()).await,
            SecretCommands::Mirror(args) => mirror_all(registry, &args).instrument(span.clone()).await,
        };

        match result {
            Ok(code) => code,
            Err(message) => {
                record_error(&span, OTHER_ERROR);
                eprintln!("{message}");
                ExitCode::Failed
            }
        }
    }

    /// Names the backend addressed and its kind, and nothing of the address
    /// within it or the value read or written there.
    fn span(&self) -> tracing::Span {
        match self {
            SecretCommands::Get(args) => tracing::info_span!(
                "secret get",
                { SECRETS_BACKEND_NAME } = args.backend.as_deref(),
                { SECRETS_BACKEND_KIND } = Empty,
                "error.type" = Empty,
                "otel.status_code" = Empty,
            ),
            SecretCommands::Put(args) => tracing::info_span!(
                "secret put",
                { SECRETS_BACKEND_NAME } = args.common.backend.as_str(),
                { SECRETS_BACKEND_KIND } = Empty,
                "error.type" = Empty,
                "otel.status_code" = Empty,
            ),
            SecretCommands::Mirror(_) => {
                tracing::info_span!("secret mirror", "error.type" = Empty, "otel.status_code" = Empty)
            }
        }
    }
}

/// Puts the resolved backend's kind on the command span, once it is known.
fn record_kind(backend: &Backend) {
    tracing::Span::current().record(SECRETS_BACKEND_KIND, backend.kind());
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        discarded_error_source,
        reason = "This terminal CLI boundary returns a rendered error message; upstream typed errors are displayed here rather than propagated to another error consumer"
    )
)]
async fn get(registry: &Registry, args: &GetArgs) -> Result<ExitCode, String> {
    if let Some(name) = &args.name {
        return get_named(registry, args, name).await;
    }
    let raw = args.raw();
    if !args.fallbacks.is_empty() {
        return get_with_fallbacks(registry, args, &raw).await;
    }
    let backend = registry.resolve(&raw.backend).map_err(|e| e.to_string())?;
    record_kind(&backend);
    let var =
        SecretVar::resolve_at(kind_of(registry, &raw.backend)?, &raw, VarSite::Flags).map_err(|e| e.to_string())?;

    let value = read(&backend, &var).await?;

    if args.output_format.is_json() {
        return Ok(print_json(&document(&raw, &value), ExitCode::Success));
    }

    write_value(&value)?;
    Ok(ExitCode::Success)
}

/// A read that opted in to more than one address goes through the same fetch
/// a config var does, so what counts as unable to answer, what is reported and
/// what is recorded cannot differ between the two.
///
/// The first backend is not built up front: one that cannot be built on this
/// machine is exactly the case a fallback is for.
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        discarded_error_source,
        reason = "This terminal CLI boundary returns a rendered error message; upstream typed errors are displayed here rather than propagated to another error consumer"
    )
)]
async fn get_with_fallbacks(registry: &Registry, args: &GetArgs, raw: &RawSecretVar) -> Result<ExitCode, String> {
    let primary =
        SecretVar::resolve_at(kind_of(registry, &raw.backend)?, raw, VarSite::Flags).map_err(|e| e.to_string())?;
    let mut addresses = vec![primary];
    for (i, fallback) in args.fallbacks.iter().enumerate() {
        addresses.push(
            SecretVar::resolve_at(
                kind_of(registry, &fallback.backend)?,
                fallback,
                VarSite::fallback_flag(i + 1),
            )
            .map_err(|e| e.to_string())?,
        );
    }
    let read = SecretRead::from_addresses(addresses).ok_or("no address to read")?;

    let fetched = vars::fetch(registry, std::slice::from_ref(&read))
        .await
        .map_err(|e| e.to_string())?;
    let value = fetched.get(&read).ok_or("the read answered nothing")?;

    if args.output_format.is_json() {
        let mut out = document(raw, value);
        if let (Some(answered_by), Some(map)) = (fetched.answered_by(&read), out.as_object_mut()) {
            map.insert("answered_by".to_string(), Value::String(answered_by.to_string()));
        }
        return Ok(print_json(&out, ExitCode::Success));
    }

    write_value(value)?;
    Ok(ExitCode::Success)
}

/// A secret read by the name it is declared under goes through the same fetch
/// as a config var, so what falls through, what is reported and what is
/// recorded is the same as for `--fallback`.
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        discarded_error_source,
        reason = "This terminal CLI boundary returns a rendered error message; upstream typed errors are displayed here rather than propagated to another error consumer"
    )
)]
async fn get_named(registry: &Registry, args: &GetArgs, name: &SecretName) -> Result<ExitCode, String> {
    let read = registry.named_read(name).map_err(|e| e.to_string())?;
    let fetched = vars::fetch(registry, std::slice::from_ref(&read))
        .await
        .map_err(|e| e.to_string())?;
    let value = fetched.get(&read).ok_or("the read answered nothing")?;

    if args.output_format.is_json() {
        let mut map = serde_json::Map::new();
        map.insert("name".to_string(), serde_json::Value::String(name.as_str().to_string()));
        map.insert(
            "value".to_string(),
            serde_json::Value::String(value.expose_secret().to_string()),
        );
        if let Some(answered_by) = fetched.answered_by(&read) {
            map.insert(
                "answered_by".to_string(),
                serde_json::Value::String(answered_by.to_string()),
            );
        }
        let out = serde_json::Value::Object(map);
        return Ok(print_json(&out, ExitCode::Success));
    }

    write_value(value)?;
    Ok(ExitCode::Success)
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        discarded_error_source,
        reason = "This terminal CLI boundary returns a rendered error message; upstream typed errors are displayed here rather than propagated to another error consumer"
    )
)]
async fn put(registry: &Registry, args: &PutArgs) -> Result<ExitCode, String> {
    let backend = registry.resolve(&args.common.backend).map_err(|e| e.to_string())?;
    record_kind(&backend);

    // Checked before stdin is read, so a read-only backend is rejected without
    // first consuming a piped secret or prompting an interactive user for one
    // that was never going to be written.
    let kind = kind_of(registry, &args.common.backend)?;
    if !kind.is_writable() {
        return Err(format!(
            "backend `{}` is of kind `{kind}`, which `trg` only reads from; \
             its entries are managed in the product itself, so write the value there \
             and read it back with `trg secret get`",
            args.common.backend
        ));
    }

    let value = read_value()?;
    let path = SecretPath::parse(&args.path).map_err(|e| e.to_string())?;
    let key = SecretKey::parse(&args.key).map_err(|e| e.to_string())?;
    let existed = write_key(&backend, &path, key, value, args).await?;

    // The confirmation is on stderr under `text` so a `put` in a pipeline
    // leaves stdout empty. Under `json` it is the result, and moves to stdout
    // with everything else a caller parses.
    if args.common.output_format.is_json() {
        #[derive(serde::Serialize)]
        struct PutDocument<'a> {
            backend: &'a str,
            path: &'a str,
            key: &'a str,
            replaced: bool,
            declaration: String,
        }
        let document = PutDocument {
            backend: &args.common.backend,
            path: &args.path,
            key: &args.key,
            replaced: existed,
            declaration: args.raw().declaration(),
        };
        return Ok(print_json(&document, ExitCode::Success));
    }

    let verb = if existed { "replaced" } else { "wrote" };
    eprintln!("{verb} `{}` at `{}` in `{}`", args.key, args.path, args.common.backend);
    eprintln!("declare it with: {}", args.raw().declaration());
    Ok(ExitCode::Success)
}

/// The declared mirrors, in name order, or only the named ones.
///
/// A mirror that fails does not stop the ones after it, and a target that
/// fails does not stop the others of its mirror: they are independent, so one
/// unreachable backend should not hide that the rest are already in sync or
/// still drifted. The exit code still reports the failure.
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        discarded_error_source,
        reason = "This terminal CLI boundary returns a rendered error message; upstream typed errors are displayed here rather than propagated to another error consumer"
    )
)]
async fn mirror_all(registry: &Registry, mirror_args: &MirrorArgs) -> Result<ExitCode, String> {
    let apply = mirror_args.apply();
    let args = mirror_args.selection();
    let declared = registry.mirrors().map_err(|e| e.to_string())?;
    if declared.is_empty() {
        return Err("no `[secrets.mirrors.<name>]` tables are declared".to_string());
    }
    let mirrors = select_mirrors(declared, &args.names)?;

    let resolve = |name: &str| registry.resolve(name);
    let mut runs = Vec::with_capacity(mirrors.len());
    for entry in &mirrors {
        let span = tracing::info_span!(
            "mirror secret",
            { SECRETS_MIRROR_NAME } = entry.name().as_str(),
            "error.type" = Empty,
            "otel.status_code" = Empty,
        );
        let run = secrets_mirror::mirror(&resolve, entry, apply)
            .instrument(span.clone())
            .await;
        match &run {
            Err(e) => record_error(&span, e.error_type()),
            Ok(targets) if targets.iter().any(Result::is_err) => record_error(&span, "mirror_failed"),
            Ok(_) => {}
        }
        runs.push(run);
    }

    let results: Vec<Vec<TargetResult>> = mirrors
        .iter()
        .zip(&runs)
        .map(|(entry, run)| target_results(entry, run))
        .collect();
    let failed = results.iter().flatten().filter(|r| r.is_err()).count();
    if failed > 0 {
        record_error(&tracing::Span::current(), "mirror_failed");
    }
    let code = if failed > 0 {
        ExitCode::Failed
    } else {
        ExitCode::Success
    };

    if args.output_format.is_json() {
        let entries: Vec<Value> = mirrors
            .iter()
            .zip(&results)
            .map(|(entry, results)| {
                let targets: Vec<Value> = entry
                    .targets()
                    .iter()
                    .zip(results)
                    .map(|(target, result)| {
                        let mut out = serde_json::Map::new();
                        out.insert("target".to_string(), Value::String(target.var().to_string()));
                        match result {
                            Ok(outcome) => out.insert("status".to_string(), Value::String(outcome.label().to_string())),
                            Err(e) => {
                                out.insert("error".to_string(), Value::String(e.to_string()));
                                out.insert("status".to_string(), Value::String("failed".to_string()))
                            }
                        };
                        Value::Object(out)
                    })
                    .collect();
                #[derive(serde::Serialize)]
                struct MirrorEntry<'a> {
                    name: &'a str,
                    source: String,
                    targets: Vec<Value>,
                }
                serde_json::to_value(MirrorEntry {
                    name: entry.name().as_str(),
                    source: entry.source().to_string(),
                    targets,
                })
                .expect("mirror entry contains only serializable fields")
            })
            .collect();
        #[derive(serde::Serialize)]
        struct MirrorDocument {
            applied: bool,
            entries: Vec<Value>,
        }
        return Ok(print_json(
            &MirrorDocument {
                applied: apply,
                entries,
            },
            code,
        ));
    }

    for (entry, results) in mirrors.iter().zip(&results) {
        for (target, result) in entry.targets().iter().zip(results) {
            let label = result.as_ref().map_or("failed", |outcome| outcome.label());
            println!("{label:<13} {} -> {}", entry.name(), target.describe());
            if let Err(e) = result {
                eprintln!("  {e}");
            }
        }
    }
    println!("{}", mirror_summary(&results, apply));

    Ok(code)
}

type TargetResult<'a> = Result<MirrorOutcome, &'a MirrorError>;

/// One result per target, so a source that could not be read reads as every
/// one of its targets failing.
fn target_results<'a>(entry: &Mirror, run: &'a Result<TargetOutcomes, MirrorError>) -> Vec<TargetResult<'a>> {
    match run {
        Ok(targets) => targets.iter().map(|r| r.as_ref().map(|o| *o)).collect(),
        Err(e) => entry.targets().iter().map(|_| Err(e)).collect(),
    }
}

fn select_mirrors(declared: Vec<Mirror>, names: &[String]) -> Result<Vec<Mirror>, String> {
    if names.is_empty() {
        return Ok(declared);
    }
    let known = || {
        declared
            .iter()
            .map(|m| format!("`{}`", m.name()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    for name in names {
        if !declared.iter().any(|m| m.name().as_str() == name) {
            return Err(format!("`{name}` is not a declared mirror; declared: {}", known()));
        }
    }
    Ok(declared
        .into_iter()
        .filter(|m| names.iter().any(|n| n == m.name().as_str()))
        .collect())
}

fn mirror_summary(results: &[Vec<TargetResult<'_>>], apply: bool) -> String {
    let all: Vec<&TargetResult<'_>> = results.iter().flatten().collect();
    let count = |wanted: MirrorOutcome| all.iter().filter(|r| matches!(r, Ok(o) if *o == wanted)).count();
    let failed = all.iter().filter(|r| r.is_err()).count();
    let mut parts = vec![format!("{} up to date", count(MirrorOutcome::UpToDate))];
    if apply {
        parts.push(format!("{} created", count(MirrorOutcome::Created)));
        parts.push(format!("{} updated", count(MirrorOutcome::Updated)));
    } else {
        parts.push(format!("{} would create", count(MirrorOutcome::WouldCreate)));
        parts.push(format!("{} would update", count(MirrorOutcome::WouldUpdate)));
    }
    parts.push(format!("{failed} failed"));
    let noun = if all.len() == 1 { "target" } else { "targets" };
    let mut line = format!("{} {noun}: {}", all.len(), parts.join(", "));
    if !apply && all.iter().any(|r| matches!(r, Ok(o) if o.is_drift())) {
        line.push_str("; run `trg secret mirror apply` to write");
    }
    line
}

/// The declared kind of a backend that has already resolved.
fn kind_of(registry: &Registry, name: &str) -> Result<BackendKind, String> {
    registry.kind_of(name).ok_or_else(|| {
        format!(
            "`{name}` is not a declared secrets backend; declared: {}",
            registry.names()
        )
    })
}

/// The result document, carrying the address in the form it was given and no
/// null placeholder for the form it was not.
fn document(raw: &RawSecretVar, value: &SecretString) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("backend".to_string(), Value::String(raw.backend.clone()));
    for (field, given) in [
        ("path", raw.path.as_deref()),
        ("key", raw.key.as_deref()),
        ("ref", raw.reference.as_deref()),
    ] {
        if let Some(given) = given {
            out.insert(field.to_string(), Value::String(given.to_string()));
        }
    }
    out.insert("value".to_string(), Value::String(value.expose_secret().to_string()));
    Value::Object(out)
}

async fn read(backend: &Backend, var: &SecretVar) -> Result<SecretString, String> {
    match var.address() {
        SecretAddress::Keychain(r) => read_key(backend, r.path(), r.key(), var.backend()).await,
        SecretAddress::Openbao(r) => read_key(backend, r.path(), r.key(), var.backend()).await,
        SecretAddress::OnePassword(r) => read_field(backend, r, var.backend()).await,
    }
}

async fn read_key(backend: &Backend, path: &SecretPath, key: &SecretKey, name: &str) -> Result<SecretString, String> {
    let map = backend
        .get(path)
        .await
        .map_err(|e| format!("could not read `{path}` from `{name}`: {e}"))?
        .ok_or_else(|| format!("no entry at `{path}` in `{name}`"))?;

    // Key names are not secret; the values behind them are, and none of them
    // is named here.
    map.get(key).cloned().ok_or_else(|| {
        format!(
            "no key `{key}` at `{path}` (that entry holds: {})",
            map.sorted_keys()
                .iter()
                .map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        )
    })
}

/// A plain 1Password read answers with the whole item, so the field is selected out
/// of that here rather than asked for by itself. Several references into one
/// item therefore still cost one `op item get`, the same way several keys at
/// one path cost one round trip.
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        discarded_error_source,
        reason = "This terminal CLI boundary returns a rendered error message; upstream typed errors are displayed here rather than propagated to another error consumer"
    )
)]
async fn read_field(backend: &Backend, reference: &OnePasswordReference, name: &str) -> Result<SecretString, String> {
    let item = reference.item();
    if reference.query().is_some() {
        return match backend
            .read(reference)
            .await
            .map_err(|e| format!("could not read `{reference}` from `{name}`: {e}"))?
        {
            OpRead::Value(value) => Ok(value),
            OpRead::MissingItem => Err(format!("no item `{item}` in `{name}`")),
            OpRead::MissingField => Err(format!("no field at `{reference}` on item `{item}` in `{name}`")),
        };
    }
    let fields = backend
        .get_item(item)
        .await
        .map_err(|e| format!("could not read `{item}` from `{name}`: {e}"))?
        .ok_or_else(|| format!("no item `{item}` in `{name}`"))?;

    fields
        .get(reference)
        .map_err(|e| e.to_string())?
        .cloned()
        .ok_or_else(|| {
            format!(
                "no field at `{reference}` (that item holds: {})",
                fields.addresses().join(", ")
            )
        })
}

/// Read, modify, write, answering whether the key was already there.
///
/// An entry holds several keys and the others belong to whoever put them
/// there, so the whole map is carried back. Not atomic against a concurrent
/// writer, which is a race a single person at one terminal does not run into
/// and a guard against it would have to be built on KV v2's own
/// compare-and-set.
async fn write_key(
    backend: &Backend,
    path: &SecretPath,
    key: SecretKey,
    value: SecretString,
    args: &PutArgs,
) -> Result<bool, String> {
    let mut map = backend
        .get(path)
        .await
        .map_err(|e| format!("could not read `{}` from `{}`: {e}", args.path, args.common.backend))?
        .unwrap_or_else(SecretMap::new);

    let existed = map.contains_key(&key);
    map.insert(key, value);

    backend
        .set(path, &map)
        .await
        .map_err(|e| format!("could not write `{}` to `{}`: {e}", args.path, args.common.backend))?;

    Ok(existed)
}

/// Take the value from stdin, never from an argument.
///
/// A `--value` flag would put the secret in `argv`, where anything else on the
/// machine can read it out of `ps`, and in the shell history besides.
fn read_value() -> Result<SecretString, String> {
    if std::io::stdin().is_terminal() {
        return Err("the value is read from stdin, so pipe it in: \
                    `op read \"op://...\" | trg secret put ...`"
            .to_string());
    }

    let mut buf = String::new();
    tracing::info_span!("read stdin").in_scope(|| {
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("could not read the value from stdin: {e}"))
    })?;

    let value = strip_one_line_ending(&buf);

    if value.is_empty() {
        return Err("refusing to write an empty value; \
                    a command that failed upstream is the usual reason for one"
            .to_string());
    }

    Ok(SecretString::from(value.to_string()))
}

/// One trailing line ending is the shell's, not the secret's, and a CRLF is one
/// line ending rather than a newline with a stray carriage return in front of
/// it. Anything beyond that was deliberate and is left alone.
fn strip_one_line_ending(buf: &str) -> &str {
    buf.strip_suffix("\r\n")
        .or_else(|| buf.strip_suffix('\n'))
        .unwrap_or(buf)
}

/// Print the value raw, so it can be piped, with a newline only where a person
/// is looking at it.
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        discarded_error_source,
        reason = "This terminal CLI boundary returns a rendered error message; upstream typed errors are displayed here rather than propagated to another error consumer"
    )
)]
fn write_value(value: &SecretString) -> Result<(), String> {
    let mut out = std::io::stdout();
    let terminating = if out.is_terminal() { "\n" } else { "" };
    write!(out, "{}{terminating}", value.expose_secret()).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}

#[cfg(test)]
#[path = "secret_tests.rs"]
mod tests;
