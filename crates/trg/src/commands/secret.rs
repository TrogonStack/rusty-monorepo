//! `trg secret get`, `trg secret put` and `trg secret mirror`.
//!
//! A secret has an identity of its own rather than one borrowed from whichever
//! MCP server happens to read it, so the arguments here are the inline table
//! from `[mcp.servers.<name>.vars]` spelled out on a command line: the same
//! backend name, addressed in the same vocabulary. A value that reads here
//! reads there, and a var that is refused here is refused at load too, because
//! both go through [`SecretVar::resolve_at`].

use std::io::{IsTerminal, Read, Write};

use clap::{Args, Subcommand};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};
use tracing::field::Empty;
use tracing::Instrument;

use crate::config::{RawSecretVar, SecretRead, SecretVar, VarSite};
use crate::exit_code::ExitCode;
use crate::output::{print_json, OutputFormat};
use crate::secrets::mirror as secrets_mirror;
use crate::secrets::onepassword::{OnePasswordReference, OpRead};
use crate::secrets::telemetry::record_error;
use crate::secrets::{vars, Backend, BackendKind, Registry, SecretAddress, SecretKey, SecretMap, SecretPath};
use crate::secrets::{Mirror, MirrorError, MirrorOutcome, TargetOutcomes};
use crate::telemetry::semconv::trg::SECRETS_MIRROR_NAME;
use crate::telemetry::semconv::trg::{SECRETS_BACKEND_KIND, SECRETS_BACKEND_NAME};

/// `error.type` for a failure this command only has as a message to print,
/// the conventions' own fallback value.
const OTHER_ERROR: &str = "_OTHER";

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
#[group(required = true, multiple = true)]
pub struct AddressArgs {
    /// The entry within that backend, relative to its configured prefix
    #[arg(long, requires = "key", conflicts_with = "reference")]
    pub path: Option<String>,

    /// The field within that entry
    #[arg(long, requires = "path", conflicts_with = "reference")]
    pub key: Option<String>,

    /// A 1Password secret reference, as `Copy Secret Reference` yields it
    #[arg(long = "ref", value_name = "op://...")]
    pub reference: Option<String>,
}

#[derive(Args)]
pub struct GetArgs {
    #[command(flatten)]
    pub common: CommonArgs,

    #[command(flatten)]
    pub address: AddressArgs,

    /// Another address to read when this one cannot answer, written as the
    /// inline table a config holds, for example `'{ backend = "op", ref =
    /// "op://<vault>/<item>/<field>" }'`. Repeat to add more, tried in order.
    /// Which backend answered is reported on stderr, never on stdout.
    #[arg(long = "fallback", value_name = "TABLE", value_parser = parse_fallback)]
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
            backend: self.common.backend.clone(),
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
                { SECRETS_BACKEND_NAME } = args.common.backend.as_str(),
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

async fn get(registry: &Registry, args: &GetArgs) -> Result<ExitCode, String> {
    let raw = args.raw();
    if !args.fallbacks.is_empty() {
        return get_with_fallbacks(registry, args, &raw).await;
    }
    let backend = registry.resolve(&raw.backend).map_err(|e| e.to_string())?;
    record_kind(&backend);
    let var =
        SecretVar::resolve_at(kind_of(registry, &raw.backend)?, &raw, VarSite::Flags).map_err(|e| e.to_string())?;

    let value = read(&backend, &var).await?;

    if args.common.output_format.is_json() {
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

    if args.common.output_format.is_json() {
        let mut out = document(raw, value);
        if let (Some(answered_by), Some(map)) = (fetched.answered_by(&read), out.as_object_mut()) {
            map.insert("answered_by".to_string(), json!(answered_by));
        }
        return Ok(print_json(&out, ExitCode::Success));
    }

    write_value(value)?;
    Ok(ExitCode::Success)
}

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
        let document = json!({
            "backend": args.common.backend,
            "path": args.path,
            "key": args.key,
            "replaced": existed,
            "declaration": args.raw().declaration(),
        });
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
                        out.insert("target".to_string(), json!(target.var().to_string()));
                        match result {
                            Ok(outcome) => out.insert("status".to_string(), json!(outcome.label())),
                            Err(e) => {
                                out.insert("error".to_string(), json!(e.to_string()));
                                out.insert("status".to_string(), json!("failed"))
                            }
                        };
                        Value::Object(out)
                    })
                    .collect();
                json!({
                    "name": entry.name().as_str(),
                    "source": entry.source().to_string(),
                    "targets": targets,
                })
            })
            .collect();
        return Ok(print_json(&json!({ "applied": apply, "entries": entries }), code));
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
    out.insert("backend".to_string(), json!(raw.backend));
    for (field, given) in [
        ("path", raw.path.as_deref()),
        ("key", raw.key.as_deref()),
        ("ref", raw.reference.as_deref()),
    ] {
        if let Some(given) = given {
            out.insert(field.to_string(), json!(given));
        }
    }
    out.insert("value".to_string(), json!(value.expose_secret()));
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
fn write_value(value: &SecretString) -> Result<(), String> {
    let mut out = std::io::stdout();
    let terminating = if out.is_terminal() { "\n" } else { "" };
    write!(out, "{}{terminating}", value.expose_secret()).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::fake::FakeBackend;
    use crate::secrets::KeychainReference;
    use clap::{CommandFactory, Parser};

    /// A parser holding just the secret subcommands, so the flag grammar can
    /// be exercised without going through the whole binary's argv.
    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        command: SecretCommands,
    }

    fn parse(argv: &[&str]) -> Result<SecretCommands, clap::Error> {
        Cli::try_parse_from(std::iter::once("trg").chain(argv.iter().copied())).map(|c| c.command)
    }

    fn put_args(key: &str) -> PutArgs {
        PutArgs {
            common: CommonArgs {
                backend: "fake".to_string(),
                output_format: OutputFormat::Text,
            },
            path: "mcp/demo".to_string(),
            key: key.to_string(),
        }
    }

    fn path() -> SecretPath {
        SecretPath::parse("mcp/demo").expect("path")
    }

    fn key(name: &str) -> SecretKey {
        SecretKey::parse(name).expect("key")
    }

    fn keychain_var(name: &str) -> SecretVar {
        SecretVar::new(
            "fake".to_string(),
            SecretAddress::Keychain(KeychainReference::new(path(), key(name))),
        )
    }

    async fn write(backend: &Backend, name: &str, value: &str) -> bool {
        write_key(
            backend,
            &path(),
            key(name),
            SecretString::from(value.to_string()),
            &put_args(name),
        )
        .await
        .expect("write")
    }

    #[test]
    fn the_flag_grammar_is_well_formed() {
        Cli::command().debug_assert();
    }

    fn get_args(argv: &[&str]) -> Result<GetArgs, clap::Error> {
        let mut full = vec![
            "get",
            "--backend",
            "homelab",
            "--path",
            "exec/github",
            "--key",
            "GH_TOKEN",
        ];
        full.extend_from_slice(argv);
        match parse(&full)? {
            SecretCommands::Get(args) => Ok(args),
            _ => panic!("expected get"),
        }
    }

    #[test]
    fn get_takes_no_fallback_unless_asked_to() {
        assert!(get_args(&[]).unwrap().fallbacks.is_empty());
    }

    #[test]
    fn get_takes_fallbacks_in_the_order_given() {
        let args = get_args(&[
            "--fallback",
            r#"{ backend = "op", ref = "op://Ops/github/GH_TOKEN" }"#,
            "--fallback",
            r#"{ backend = "other", path = "exec/github", key = "GH_TOKEN" }"#,
        ])
        .unwrap();

        assert_eq!(args.fallbacks.len(), 2);
        assert_eq!(args.fallbacks[0].backend, "op");
        assert_eq!(args.fallbacks[1].backend, "other");
    }

    #[test]
    fn a_fallback_that_is_not_an_address_table_is_refused_at_parse_time() {
        assert!(get_args(&["--fallback", "not a table"]).is_err());
        assert!(get_args(&["--fallback", r#"{ backend = "op", typo = true }"#]).is_err());
    }

    #[test]
    fn put_has_no_fallback_flag() {
        assert!(parse(&["put", "--backend", "b", "--path", "p", "--key", "k", "--fallback", "{}"]).is_err());
    }

    #[tokio::test]
    async fn a_first_write_reports_that_nothing_was_there() {
        let backend = Backend::Fake(FakeBackend::new());
        assert!(!write(&backend, "token", "one").await);
    }

    #[tokio::test]
    async fn writing_over_a_key_reports_the_replacement() {
        let backend = Backend::Fake(FakeBackend::new());
        write(&backend, "token", "one").await;
        assert!(write(&backend, "token", "two").await);

        let got = read(&backend, &keychain_var("token")).await.expect("read");
        assert_eq!(got.expose_secret(), "two");
    }

    #[tokio::test]
    async fn a_sibling_key_survives_a_write_next_to_it() {
        let backend = Backend::Fake(FakeBackend::new());
        write(&backend, "token", "one").await;
        write(&backend, "refresh", "two").await;

        let token = read(&backend, &keychain_var("token")).await.expect("read");
        assert_eq!(token.expose_secret(), "one");
    }

    #[tokio::test]
    async fn an_unwritten_path_is_named_rather_than_read_as_empty() {
        let backend = Backend::Fake(FakeBackend::new());
        let err = read(&backend, &keychain_var("token")).await.expect_err("no entry");
        assert!(err.contains("no entry at `mcp/demo`"), "{err}");
    }

    #[tokio::test]
    async fn a_missing_key_names_its_siblings_and_no_value() {
        let backend = Backend::Fake(FakeBackend::new());
        write(&backend, "token", "super-secret").await;
        write(&backend, "refresh", "also-secret").await;

        let err = read(&backend, &keychain_var("nope")).await.expect_err("no key");
        assert!(err.contains("refresh"), "{err}");
        assert!(err.contains("token"), "{err}");
        assert!(!err.contains("super-secret"), "{err}");
        assert!(!err.contains("also-secret"), "{err}");
    }

    #[tokio::test]
    async fn a_query_reference_tells_a_missing_field_apart_from_a_missing_item() {
        let backend = Backend::Fake(FakeBackend::new());
        let mut map = SecretMap::new();
        map.insert(key("TOKEN"), SecretString::from("super-secret".to_string()));
        backend
            .set(&SecretPath::parse("Ops/deploy-keys").expect("path"), &map)
            .await
            .expect("seed");
        let var = |raw: &str| {
            SecretVar::new(
                "fake".to_string(),
                SecretAddress::OnePassword(Box::new(OnePasswordReference::parse(raw).expect("reference"))),
            )
        };

        let field = read(&backend, &var("op://Ops/deploy-keys/nope?ssh-format=openssh"))
            .await
            .expect_err("no field");
        assert!(field.contains("no field at"), "{field}");
        assert!(!field.contains("no item"), "{field}");

        let item = read(&backend, &var("op://Ops/absent/TOKEN?ssh-format=openssh"))
            .await
            .expect_err("no item");
        assert!(item.contains("no item `"), "{item}");
    }

    /// A 1Password item answers with every field at once, so a reference that
    /// resolves to nothing has the rest of the item to list as the hint.
    #[tokio::test]
    async fn a_reference_reads_the_field_it_names_and_names_the_others_when_it_misses() {
        let backend = Backend::Fake(FakeBackend::new());
        let mut map = SecretMap::new();
        map.insert(key("TOKEN"), SecretString::from("super-secret".to_string()));
        map.insert(key("Prod/TOKEN"), SecretString::from("also-secret".to_string()));
        backend
            .set(&SecretPath::parse("Ops/deploy-keys").expect("path"), &map)
            .await
            .expect("seed");

        let var = |raw: &str| {
            SecretVar::new(
                "fake".to_string(),
                SecretAddress::OnePassword(Box::new(OnePasswordReference::parse(raw).expect("reference"))),
            )
        };

        let plain = read(&backend, &var("op://Ops/deploy-keys/TOKEN")).await.expect("read");
        assert_eq!(plain.expose_secret(), "super-secret");

        let sectioned = read(&backend, &var("op://Ops/deploy-keys/Prod/TOKEN"))
            .await
            .expect("read");
        assert_eq!(sectioned.expose_secret(), "also-secret");

        let err = read(&backend, &var("op://Ops/deploy-keys/ABSENT"))
            .await
            .expect_err("no such field");
        assert!(err.contains("Prod/TOKEN"), "{err}");
        assert!(!err.contains("super-secret"), "{err}");
    }

    /// The document names the address the way it was given, so a script that
    /// reads one back is not left guessing which of two forms it holds.
    #[test]
    fn the_json_document_carries_only_the_form_that_was_used() {
        let value = SecretString::from("super-secret".to_string());

        let by_path = document(&put_args("token").raw(), &value);
        assert_eq!(by_path["path"], json!("mcp/demo"));
        assert_eq!(by_path["value"], json!("super-secret"));
        assert!(by_path.get("ref").is_none());

        let by_reference = document(
            &RawSecretVar {
                backend: "op".to_string(),
                path: None,
                key: None,
                reference: Some("op://Ops/deploy-keys/TOKEN".to_string()),
            },
            &value,
        );
        assert_eq!(by_reference["ref"], json!("op://Ops/deploy-keys/TOKEN"));
        assert!(by_reference.get("path").is_none());
        assert!(by_reference.get("key").is_none());
    }

    #[test]
    fn a_declaration_can_be_pasted_into_a_vars_table() {
        let line = put_args("token").raw().declaration();
        assert_eq!(line, r#"{ backend = "fake", path = "mcp/demo", key = "token" }"#);
    }

    /// The two vocabularies are not mixable, and clap is what says so, before
    /// anything has looked up what kind of backend was named.
    #[test]
    fn a_reference_and_a_path_cannot_be_given_together() {
        assert!(parse(&[
            "get",
            "--backend",
            "op",
            "--ref",
            "op://Ops/deploy-keys/TOKEN",
            "--path",
            "Ops/deploy-keys",
            "--key",
            "TOKEN",
        ])
        .is_err());
    }

    #[test]
    fn half_of_a_path_and_key_address_is_refused() {
        assert!(parse(&["get", "--backend", "local", "--path", "mcp/demo"]).is_err());
        assert!(parse(&["get", "--backend", "local", "--key", "token"]).is_err());
    }

    #[test]
    fn an_address_is_required() {
        assert!(parse(&["get", "--backend", "local"]).is_err());
    }

    /// `put` writes, and the only backend addressed by reference is one `trg`
    /// cannot write to, so the flag is not on this subcommand at all.
    #[test]
    fn put_takes_no_reference() {
        assert!(parse(&["put", "--backend", "op", "--ref", "op://Ops/deploy-keys/TOKEN"]).is_err());
    }

    #[test]
    fn both_forms_parse_on_their_own() {
        assert!(parse(&["get", "--backend", "local", "--path", "mcp/demo", "--key", "token"]).is_ok());
        assert!(parse(&["get", "--backend", "op", "--ref", "op://Ops/deploy-keys/TOKEN"]).is_ok());
    }

    #[test]
    fn a_mirror_is_a_dry_run_unless_it_is_told_to_apply() {
        let Ok(SecretCommands::Mirror(dry)) = parse(&["mirror"]) else {
            panic!("`mirror` should parse")
        };
        assert!(!dry.apply(), "nothing is written by default");

        let Ok(SecretCommands::Mirror(applied)) = parse(&["mirror", "apply"]) else {
            panic!("`mirror apply` should parse")
        };
        assert!(applied.apply(), "apply is what writes");
    }

    #[test]
    fn the_apply_flag_is_not_accepted() {
        assert!(parse(&["mirror", "--apply"]).is_err());
    }

    #[test]
    fn a_mirror_can_be_picked_by_name() {
        let Ok(SecretCommands::Mirror(dry)) = parse(&["mirror", "--name", "a", "--name", "b"]) else {
            panic!("names should parse")
        };
        assert!(!dry.apply());
        assert_eq!(dry.selection().names, ["a", "b"]);

        let Ok(SecretCommands::Mirror(applied)) = parse(&["mirror", "apply", "--name", "a", "--name", "b"]) else {
            panic!("names should parse after apply")
        };
        assert!(applied.apply());
        assert_eq!(applied.selection().names, ["a", "b"]);
    }

    /// A name is a flag value, so a mirror declared as `apply` is picked for a
    /// preview rather than read as the subcommand that writes.
    #[test]
    fn a_mirror_named_like_a_subcommand_is_still_only_previewed() {
        let Ok(SecretCommands::Mirror(dry)) = parse(&["mirror", "--name", "apply"]) else {
            panic!("`--name apply` should parse")
        };
        assert!(!dry.apply());
        assert_eq!(dry.selection().names, ["apply"]);
    }

    #[test]
    fn a_name_is_not_taken_positionally() {
        assert!(parse(&["mirror", "a"]).is_err());
    }

    fn declared() -> Vec<Mirror> {
        let section: crate::secrets::SecretsSection = toml::from_str(
            r#"
            [backends.src]
            kind = "keychain"

            [backends.dst]
            kind = "keychain"

            [mirrors.zeta]
            source = { backend = "src", path = "a", key = "k" }
            targets = [{ backend = "dst", path = "z", key = "k" }]

            [mirrors.alpha]
            source = { backend = "src", path = "a", key = "k" }
            targets = [{ backend = "dst", path = "al", key = "k" }]

            [mirrors.mid]
            source = { backend = "src", path = "a", key = "k" }
            targets = [{ backend = "dst", path = "m", key = "k" }]
            "#,
        )
        .expect("section");
        section.resolve_mirrors().expect("mirrors")
    }

    fn names(mirrors: &[Mirror]) -> Vec<String> {
        mirrors.iter().map(|m| m.name().to_string()).collect()
    }

    #[test]
    fn no_names_selects_every_mirror_in_sorted_order() {
        let picked = select_mirrors(declared(), &[]).unwrap();
        assert_eq!(names(&picked), ["alpha", "mid", "zeta"]);
    }

    #[test]
    fn a_subset_keeps_sorted_order_whatever_order_it_was_named_in() {
        let picked = select_mirrors(declared(), &["zeta".to_string(), "alpha".to_string()]).unwrap();
        assert_eq!(names(&picked), ["alpha", "zeta"]);
    }

    #[test]
    fn a_name_given_twice_runs_its_mirror_once() {
        let picked = select_mirrors(declared(), &["mid".to_string(), "mid".to_string()]).unwrap();
        assert_eq!(names(&picked), ["mid"]);
    }

    #[test]
    fn an_unknown_name_is_an_error_that_lists_the_declared_names() {
        let err = select_mirrors(declared(), &["alpha".to_string(), "ghost".to_string()]).unwrap_err();
        assert!(err.contains("`ghost`"), "{err}");
        assert!(err.contains("`alpha`, `mid`, `zeta`"), "{err}");
    }

    #[test]
    fn the_summary_counts_what_each_target_did_and_points_at_apply_only_when_it_would_help() {
        let broken = MirrorError::EmptySource;
        let results = vec![
            vec![Ok(MirrorOutcome::UpToDate), Ok(MirrorOutcome::WouldCreate)],
            vec![Ok(MirrorOutcome::WouldUpdate), Err(&broken)],
        ];
        assert_eq!(
            mirror_summary(&results, false),
            "4 targets: 1 up to date, 1 would create, 1 would update, 1 failed; run `trg secret mirror apply` to write"
        );

        let applied = vec![vec![Ok(MirrorOutcome::Created), Ok(MirrorOutcome::Updated)]];
        assert_eq!(
            mirror_summary(&applied, true),
            "2 targets: 0 up to date, 1 created, 1 updated, 0 failed"
        );

        let settled = vec![vec![Ok(MirrorOutcome::UpToDate)]];
        assert_eq!(
            mirror_summary(&settled, false),
            "1 target: 1 up to date, 0 would create, 0 would update, 0 failed"
        );
    }

    #[test]
    fn one_line_ending_comes_off_and_no_more() {
        assert_eq!(strip_one_line_ending("secret\n"), "secret");
        assert_eq!(strip_one_line_ending("secret\r\n"), "secret");
        assert_eq!(strip_one_line_ending("secret"), "secret");
        assert_eq!(strip_one_line_ending("secret\n\n"), "secret\n");
        assert_eq!(strip_one_line_ending("a\nb\n"), "a\nb");
    }

    /// A carriage return that is not part of a line ending was in the value.
    #[test]
    fn a_lone_carriage_return_is_left_where_it_is() {
        assert_eq!(strip_one_line_ending("secret\r"), "secret\r");
    }

    #[test]
    fn a_command_is_one_span_naming_its_backend_and_nothing_it_addressed() {
        use crate::secrets::telemetry::testing::{assert_never_recorded, current_thread};
        use opentelemetry::trace::Status;

        const TOKEN: &str = "secret-span-token-8c3d";
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind")
            .local_addr()
            .expect("addr")
            .port();
        let section: crate::secrets::SecretsSection = toml::from_str(&format!(
            r#"
            [backends.work]
            kind = "openbao"
            addr = "http://127.0.0.1:{port}"
            mount = "secret"
            path_prefix = "trg"
            owner = "yordis"
            token = "{TOKEN}"
            timeout_ms = 500
            "#
        ))
        .expect("section");
        let registry = Registry::new(section);
        let command = parse(&["get", "--backend", "work", "--path", "deploy-keys", "--key", "token"]).expect("parse");

        let (code, trace) = crate::telemetry::testing::capture(|| current_thread(command.handle(&registry)));
        assert_eq!(code, ExitCode::Failed, "nothing listens there");

        let span = trace.span("secret get").expect("the command span");
        assert!(matches!(span.status, Status::Error { .. }));
        let attribute = |key: &str| trace.attribute("secret get", key).map(|v| v.to_string());
        assert_eq!(attribute("trg.secrets.backend.name").as_deref(), Some("work"));
        assert_eq!(attribute("trg.secrets.backend.kind").as_deref(), Some("openbao"));
        trace.assert_child_of("GET /v1/{mount}/data/{path}", "secret get");

        assert_never_recorded(&trace, &[TOKEN, "deploy-keys", "yordis"]);
    }
}
