//! Every TOML example in the crate's docs, parsed by the loader that reads the
//! real config file.
//!
//! A config example is an instruction, so one that cannot be pasted is worse
//! than no example: it sends someone off to debug their own typing. These
//! examples went stale silently because nothing read them, and the loader is
//! the only thing that can say whether they are still true.
//!
//! A block carrying a table header claims to be a config and is loaded as one.
//! A block without one is showing the shape of a single value, and is held to
//! TOML syntax alone. An excerpt that has a header but still leans on a sibling
//! block for the rest of itself says so on the line above its fence:
//!
//! ```markdown
//! <!-- trg-example: fragment -->
//! <!-- trg-example: skip -->
//! ```
//!
//! `skip` is for a block that is a shape rather than a config, with
//! `<placeholder>` where the values go, and is held to nothing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use toml::Value;

use super::FileRoot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Full,
    Fragment,
    Skip,
}

impl Mode {
    fn parse(directive: &str) -> Option<Self> {
        match directive {
            "fragment" => Some(Self::Fragment),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct Block {
    file: PathBuf,
    /// The line the fence opens on, so a failure points at the source rather
    /// than at an ordinal the reader would have to count out by hand.
    line: usize,
    mode: Mode,
    body: String,
}

impl Block {
    fn where_(&self) -> String {
        format!("{}:{}", self.file.display(), self.line)
    }
}

/// The crate root, since tests run from wherever cargo felt like putting them.
fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn markdown_files() -> Vec<PathBuf> {
    let root = crate_root();
    let mut out = vec![root.join("README.md")];
    collect_markdown(&root.join("docs"), &mut out);
    out.sort();
    out
}

fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_markdown(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
}

fn extract_blocks(file: &Path) -> Vec<Block> {
    let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
    let lines: Vec<&str> = text.lines().collect();

    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].trim_start() != "```toml" {
            i += 1;
            continue;
        }

        let declared_mode = i.checked_sub(1).and_then(|prev| directive(lines[prev]));

        let fence_line = i + 1;
        let mut body = String::new();
        i += 1;
        while i < lines.len() && !lines[i].trim_start().starts_with("```") {
            body.push_str(lines[i]);
            body.push('\n');
            i += 1;
        }
        i += 1;

        let mode = declared_mode.unwrap_or_else(|| {
            if has_table_header(&body) {
                Mode::Full
            } else {
                Mode::Fragment
            }
        });

        blocks.push(Block {
            file: file.to_path_buf(),
            line: fence_line,
            mode,
            body,
        });
    }
    blocks
}

/// Whether the block opens a table, which is what separates a config from a
/// snippet showing what one field may look like.
fn has_table_header(body: &str) -> bool {
    body.lines().any(|l| {
        let l = l.trim();
        l.starts_with('[') && l.ends_with(']')
    })
}

fn directive(line: &str) -> Option<Mode> {
    let rest = line.trim().strip_prefix("<!-- trg-example:")?;
    Mode::parse(rest.strip_suffix("-->")?.trim())
}

/// The `[secrets.backends]` keys a block declares.
fn declared_backends(table: &Value, into: &mut BTreeSet<String>) {
    let Some(backends) = table.get("secrets").and_then(|s| s.get("backends")) else {
        return;
    };
    let Some(map) = backends.as_table() else { return };
    into.extend(map.keys().cloned());
}

/// Every backend name a block addresses, whether through a server's `secrets`
/// or through a `{ backend = ... }` var.
fn referenced_backends(table: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(servers) = table
        .get("mcp")
        .and_then(|m| m.get("servers"))
        .and_then(Value::as_table)
    else {
        return out;
    };

    for (server, entry) in servers {
        if let Some(name) = entry.get("secrets").and_then(Value::as_str) {
            out.push((format!("`[mcp.servers.{server}]` `secrets`"), name.to_string()));
        }
        let Some(vars) = entry.get("vars").and_then(Value::as_table) else {
            continue;
        };
        for (var, source) in vars {
            if let Some(name) = source.get("backend").and_then(Value::as_str) {
                out.push((format!("`[mcp.servers.{server}.vars]` `{var}`"), name.to_string()));
            }
        }
    }
    out
}

/// Every `{ var = "..." }` a server's `url` or headers name, against the `vars`
/// that server declares.
///
/// Checked per block rather than per document, because a `vars` table travels
/// with the server that owns it and nothing outside can contribute to it.
fn check_var_references(table: &Value, at: &str) {
    let Some(servers) = table
        .get("mcp")
        .and_then(|m| m.get("servers"))
        .and_then(Value::as_table)
    else {
        return;
    };

    for (server, entry) in servers {
        let declared: BTreeSet<&str> = entry
            .get("vars")
            .and_then(Value::as_table)
            .map(|t| t.keys().map(String::as_str).collect())
            .unwrap_or_default();

        let mut used: Vec<(String, String)> = Vec::new();
        if let Some(url) = entry.get("url") {
            collect_var_refs(url, &format!("`[mcp.servers.{server}]` `url`"), &mut used);
        }
        if let Some(headers) = entry.get("headers").and_then(Value::as_table) {
            for (name, value) in headers {
                collect_var_refs(value, &format!("`[mcp.servers.{server}.headers]` `{name}`"), &mut used);
            }
        }

        for (site, name) in used {
            assert!(
                declared.contains(name.as_str()),
                "{at}: {site} names var `{name}`, which `[mcp.servers.{server}.vars]` \
                 does not declare (declared: {})",
                if declared.is_empty() {
                    "none".to_string()
                } else {
                    declared.iter().copied().collect::<Vec<_>>().join(", ")
                }
            );
        }
    }
}

fn collect_var_refs(value: &Value, site: &str, out: &mut Vec<(String, String)>) {
    match value {
        Value::String(_) => {}
        Value::Table(_) => {
            if let Some(name) = value.get("var").and_then(Value::as_str) {
                out.push((site.to_string(), name.to_string()));
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_var_refs(item, site, out);
            }
        }
        _ => {}
    }
}

/// The whole point: a full example goes through the same deserialisation the
/// real config file does, `deny_unknown_fields` and required fields included.
fn check_parses_as_config(block: &Block) {
    if let Err(e) = toml::from_str::<FileRoot>(&block.body) {
        panic!(
            "{}: this example does not load as a config: {e}\n\
             If it is a deliberate excerpt, mark it `<!-- trg-example: fragment -->`.\n\
             ---\n{}---",
            block.where_(),
            block.body
        );
    }
}

#[test]
fn every_documented_config_example_still_loads() {
    let files = markdown_files();
    let mut total = 0usize;

    for file in &files {
        let blocks = extract_blocks(file);
        if blocks.is_empty() {
            continue;
        }

        let mut declared: BTreeSet<String> = BTreeSet::new();
        let mut parsed: Vec<(&Block, Value)> = Vec::new();

        for block in &blocks {
            total += 1;
            if block.mode == Mode::Skip {
                continue;
            }

            let value: Value = toml::from_str(&block.body).unwrap_or_else(|e| {
                panic!(
                    "{}: this example is not valid TOML: {e}\n---\n{}---",
                    block.where_(),
                    block.body
                )
            });
            declared_backends(&value, &mut declared);
            parsed.push((block, value));
        }

        // A document that declares no backend is talking about a hypothetical
        // one, and has made no claim for this to check against.
        let cross_check_backends = !declared.is_empty();

        for (block, value) in &parsed {
            if block.mode == Mode::Full {
                check_parses_as_config(block);
            }
            check_var_references(value, &block.where_());

            if !cross_check_backends {
                continue;
            }

            for (site, name) in referenced_backends(value) {
                assert!(
                    declared.contains(&name),
                    "{}: {site} addresses backend `{name}`, which no example in {} declares \
                     (declared: {})",
                    block.where_(),
                    file.display(),
                    declared.iter().cloned().collect::<Vec<_>>().join(", ")
                );
            }
        }
    }

    // A harness that silently stops finding examples passes for the wrong
    // reason, and would keep passing as they rotted.
    assert!(
        total >= 22,
        "expected the docs to still hold their TOML examples, found {total}"
    );
}
