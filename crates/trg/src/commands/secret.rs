//! `trg secret get` and `trg secret put`.
//!
//! A secret is addressed by the same three coordinates a config var uses, and
//! by nothing else. It has an identity of its own rather than one borrowed
//! from whichever MCP server happens to read it, so the arguments here are the
//! inline table from `[mcp.servers.<name>.vars]` spelled out on a command line.

use std::io::{IsTerminal, Read, Write};

use clap::{Args, Subcommand};
use secrecy::{ExposeSecret, SecretString};

use crate::config::SecretVar;
use crate::secrets::{Registry, SecretKey, SecretMap, SecretPath};

#[derive(Subcommand)]
pub enum SecretCommands {
    /// Read one value out of a secrets backend
    Get(SecretArgs),
    /// Write one value into a secrets backend, reading it from stdin
    Put(SecretArgs),
}

#[derive(Args)]
pub struct SecretArgs {
    /// The `[secrets.backends.<name>]` entry to address
    #[arg(long)]
    pub backend: String,

    /// The entry within that backend, relative to its configured prefix
    #[arg(long)]
    pub path: String,

    /// The field within that entry
    #[arg(long)]
    pub key: String,
}

impl SecretArgs {
    fn var(&self) -> SecretVar {
        SecretVar {
            backend: self.backend.clone(),
            path: self.path.clone(),
            key: self.key.clone(),
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
            Ok(()) => 0,
            Err(message) => {
                eprintln!("{message}");
                1
            }
        }
    }
}

async fn get(registry: &Registry, args: &SecretArgs) -> Result<(), String> {
    let (backend, path, key) = address(registry, args)?;
    let value = read_key(&backend, &path, &key, args).await?;
    write_value(&value)
}

async fn put(registry: &Registry, args: &SecretArgs) -> Result<(), String> {
    let value = read_value()?;
    let (backend, path, key) = address(registry, args)?;
    let existed = write_key(&backend, &path, key, value, args).await?;

    let verb = if existed { "replaced" } else { "wrote" };
    eprintln!("{verb} `{}` at `{}` in `{}`", args.key, args.path, args.backend);
    eprintln!("declare it with: {}", args.var().declaration());
    Ok(())
}

async fn read_key(
    backend: &crate::secrets::Backend,
    path: &SecretPath,
    key: &SecretKey,
    args: &SecretArgs,
) -> Result<SecretString, String> {
    let map = backend
        .get(path)
        .await
        .map_err(|e| format!("could not read `{}` from `{}`: {e}", args.path, args.backend))?
        .ok_or_else(|| format!("no entry at `{}` in `{}`", args.path, args.backend))?;

    // Key names are not secret; the values behind them are, and none of them
    // is named here.
    map.get(key).cloned().ok_or_else(|| {
        format!(
            "no key `{}` at `{}` (that entry holds: {})",
            args.key,
            args.path,
            map.sorted_keys()
                .iter()
                .map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(", "),
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
    backend: &crate::secrets::Backend,
    path: &SecretPath,
    key: SecretKey,
    value: SecretString,
    args: &SecretArgs,
) -> Result<bool, String> {
    let mut map = backend
        .get(path)
        .await
        .map_err(|e| format!("could not read `{}` from `{}`: {e}", args.path, args.backend))?
        .unwrap_or_else(SecretMap::new);

    let existed = map.contains_key(&key);
    map.insert(key, value);

    backend
        .set(path, &map)
        .await
        .map_err(|e| format!("could not write `{}` to `{}`: {e}", args.path, args.backend))?;

    Ok(existed)
}

type Address = (crate::secrets::Backend, SecretPath, SecretKey);

fn address(registry: &Registry, args: &SecretArgs) -> Result<Address, String> {
    let backend = registry.resolve(&args.backend).map_err(|e| e.to_string())?;
    let path = SecretPath::parse(&args.path).map_err(|e| e.to_string())?;
    let key = SecretKey::parse(&args.key).map_err(|e| e.to_string())?;
    Ok((backend, path, key))
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
    use crate::secrets::Backend;

    fn args(key: &str) -> SecretArgs {
        SecretArgs {
            backend: "fake".to_string(),
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

    async fn write(backend: &Backend, name: &str, value: &str) -> bool {
        write_key(
            backend,
            &path(),
            key(name),
            SecretString::from(value.to_string()),
            &args(name),
        )
        .await
        .expect("write")
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

        let got = read_key(&backend, &path(), &key("token"), &args("token"))
            .await
            .expect("read");
        assert_eq!(got.expose_secret(), "two");
    }

    #[tokio::test]
    async fn a_sibling_key_survives_a_write_next_to_it() {
        let backend = Backend::Fake(FakeBackend::new());
        write(&backend, "token", "one").await;
        write(&backend, "refresh", "two").await;

        let token = read_key(&backend, &path(), &key("token"), &args("token"))
            .await
            .expect("read");
        assert_eq!(token.expose_secret(), "one");
    }

    #[tokio::test]
    async fn an_unwritten_path_is_named_rather_than_read_as_empty() {
        let backend = Backend::Fake(FakeBackend::new());
        let err = read_key(&backend, &path(), &key("token"), &args("token"))
            .await
            .expect_err("no entry");
        assert!(err.contains("no entry at `mcp/demo`"), "{err}");
    }

    #[tokio::test]
    async fn a_missing_key_names_its_siblings_and_no_value() {
        let backend = Backend::Fake(FakeBackend::new());
        write(&backend, "token", "super-secret").await;
        write(&backend, "refresh", "also-secret").await;

        let err = read_key(&backend, &path(), &key("nope"), &args("nope"))
            .await
            .expect_err("no key");
        assert!(err.contains("refresh"), "{err}");
        assert!(err.contains("token"), "{err}");
        assert!(!err.contains("super-secret"), "{err}");
        assert!(!err.contains("also-secret"), "{err}");
    }

    #[test]
    fn a_declaration_can_be_pasted_into_a_vars_table() {
        let line = args("token").var().declaration();
        assert_eq!(line, r#"{ backend = "fake", path = "mcp/demo", key = "token" }"#);
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
