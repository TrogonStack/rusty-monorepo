//! `trg secret get` and `trg secret put`.
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

use crate::config::{RawSecretVar, SecretVar, VarSite};
use crate::output::{print_json, OutputFormat};
use crate::secrets::onepassword::OnePasswordReference;
use crate::secrets::{Backend, BackendKind, Registry, SecretAddress, SecretKey, SecretMap, SecretPath};

#[derive(Subcommand)]
pub enum SecretCommands {
    /// Read one value out of a secrets backend
    Get(GetArgs),
    /// Write one value into a secrets backend, reading it from stdin
    Put(PutArgs),
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

impl SecretCommands {
    pub async fn handle(self, registry: &Registry) -> i32 {
        let result = match self {
            SecretCommands::Get(args) => get(registry, &args).await,
            SecretCommands::Put(args) => put(registry, &args).await,
        };

        match result {
            Ok(code) => code,
            Err(message) => {
                eprintln!("{message}");
                1
            }
        }
    }
}

async fn get(registry: &Registry, args: &GetArgs) -> Result<i32, String> {
    let raw = args.raw();
    let backend = registry.resolve(&raw.backend).map_err(|e| e.to_string())?;
    let var =
        SecretVar::resolve_at(kind_of(registry, &raw.backend)?, &raw, VarSite::Flags).map_err(|e| e.to_string())?;

    let value = read(&backend, &var).await?;

    if args.common.output_format.is_json() {
        return Ok(print_json(&document(&raw, &value), 0));
    }

    write_value(&value)?;
    Ok(0)
}

async fn put(registry: &Registry, args: &PutArgs) -> Result<i32, String> {
    let backend = registry.resolve(&args.common.backend).map_err(|e| e.to_string())?;

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
        return Ok(print_json(&document, 0));
    }

    let verb = if existed { "replaced" } else { "wrote" };
    eprintln!("{verb} `{}` at `{}` in `{}`", args.key, args.path, args.common.backend);
    eprintln!("declare it with: {}", args.raw().declaration());
    Ok(0)
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

/// A 1Password read answers with the whole item, so the field is selected out
/// of that here rather than asked for by itself. Several references into one
/// item therefore still cost one `op item get`, the same way several keys at
/// one path cost one round trip.
async fn read_field(backend: &Backend, reference: &OnePasswordReference, name: &str) -> Result<SecretString, String> {
    let item = reference.item();
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
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| format!("could not read the value from stdin: {e}"))?;

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
}
