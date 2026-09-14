//! Every JSON example in the crate's docs and shipped skills, parsed by the code
//! that reads the real thing.
//!
//! An eval suite example is an instruction, so one that cannot be pasted is
//! worse than no example: it sends someone off to debug their own typing. The
//! suite parser and the grader parser are the only things that can say whether
//! these are still true, and until now nothing asked them.
//!
//! A block is held to whatever it claims to be. An object carrying
//! `skill_name` and `evals` claims to be a suite and is parsed as one; an
//! object carrying a string `type` claims to be a grader and is parsed as one;
//! anything else is an artifact or a shape, and is held to JSON syntax alone.
//! A block that would be read as one of the first two but is a deliberate
//! excerpt says so on its own fence:
//!
//! ````markdown
//! ```json trg-example=fragment
//! ```json trg-example=skip
//! ````
//!
//! `skip` is for a block that is a shape rather than a document, with an
//! elision or a `<placeholder>` where the rest goes, and is held to nothing.
//!
//! The marker rides on the fence rather than on a comment above it so that it
//! cannot be separated from the block it describes. GitHub takes the language
//! from the first word of an info string and drops the rest, so highlighting is
//! unaffected and the marker never renders.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::evals::{parse_eval_suite, EvalCase};
use super::graders::CaseGrader;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Suite,
    Case,
    Grader,
    Syntax,
    Skip,
}

impl Mode {
    fn parse(value: &str, at: &str) -> Self {
        match value {
            "fragment" => Self::Syntax,
            "skip" => Self::Skip,
            other => panic!("{at}: unknown `trg-example` value `{other}`; expected `fragment` or `skip`"),
        }
    }
}

#[derive(Debug)]
struct Block {
    file: PathBuf,
    /// The line the fence opens on, so a failure points at the source rather
    /// than at an ordinal the reader would have to count out by hand.
    line: usize,
    declared_mode: Option<Mode>,
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
    collect_markdown(&root.join("skills"), &mut out);
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
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();

    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(attributes) = json_fence(lines[i]) else {
            i += 1;
            continue;
        };

        let fence_line = i + 1;
        let at = format!("{}:{fence_line}", file.display());
        let declared_mode = declared_mode(&attributes, &at);

        let mut body = String::new();
        i += 1;
        while i < lines.len() && !lines[i].trim_start().starts_with("```") {
            body.push_str(lines[i]);
            body.push('\n');
            i += 1;
        }
        i += 1;

        blocks.push(Block {
            file: file.to_path_buf(),
            line: fence_line,
            declared_mode,
            body,
        });
    }
    blocks
}

/// The info string past the language, for a fence that opens a JSON block.
///
/// `None` for any other line, including a fence in another language and one
/// whose language merely starts with `json`.
fn json_fence(line: &str) -> Option<Vec<&str>> {
    let info = line.trim_start().strip_prefix("```")?;
    let mut words = info.split_whitespace();
    (words.next()? == "json").then(|| words.collect())
}

fn declared_mode(attributes: &[&str], at: &str) -> Option<Mode> {
    attributes
        .iter()
        .find_map(|a| a.strip_prefix("trg-example=").map(|value| Mode::parse(value, at)))
}

/// What the block says it is, for a block that did not say on its fence.
fn inferred_mode(value: &Value) -> Mode {
    let Some(object) = value.as_object() else {
        return match value.as_array() {
            Some(items) if !items.is_empty() && items.iter().all(is_grader_shaped) => Mode::Grader,
            _ => Mode::Syntax,
        };
    };
    if object.contains_key("skill_name") && object.contains_key("evals") {
        return Mode::Suite;
    }
    if object.contains_key("id") && object.contains_key("prompt") {
        return Mode::Case;
    }
    if object.get("type").is_some_and(Value::is_string) {
        return Mode::Grader;
    }
    Mode::Syntax
}

fn is_grader_shaped(value: &Value) -> bool {
    inferred_mode(value) == Mode::Grader
}

/// The whole point: a full example goes through the same deserialisation
/// `evals.json` does, unknown fields and required fields included.
fn check_parses_as_suite(block: &Block) {
    if let Err(e) = parse_eval_suite(&block.body) {
        panic!(
            "{}: this example does not load as an eval suite: {e}\n\
             If it is a deliberate excerpt, open its fence with: ```json trg-example=fragment\n\
             ---\n{}---",
            block.where_(),
            block.body
        );
    }
}

fn check_parses_as_case(block: &Block) {
    if let Err(e) = serde_json::from_str::<EvalCase>(&block.body) {
        panic!(
            "{}: this example does not load as an eval case: {e}\n\
             If it is a deliberate excerpt, open its fence with: ```json trg-example=fragment\n\
             ---\n{}---",
            block.where_(),
            block.body
        );
    }
}

fn check_parses_as_grader(block: &Block, value: &Value) {
    let parsed = match value.is_array() {
        true => serde_json::from_str::<Vec<CaseGrader>>(&block.body).map(|_| ()),
        false => serde_json::from_str::<CaseGrader>(&block.body).map(|_| ()),
    };
    if let Err(e) = parsed {
        panic!(
            "{}: this example does not load as a grader: {e}\n\
             If it is a deliberate excerpt, open its fence with: ```json trg-example=fragment\n\
             ---\n{}---",
            block.where_(),
            block.body
        );
    }
}

/// How many blocks of each kind the docs held when this harness was written.
///
/// A harness that silently stops recognising examples passes for the wrong reason, and
/// would keep passing as they rotted. Counting per kind rather than in total is what
/// catches the shape of a suite, a case or a grader changing underneath the inference:
/// every block would still be valid JSON, and none of them would still be checked.
const FLOORS: [(&str, Mode, usize); 3] = [
    ("eval suites", Mode::Suite, 1),
    ("eval cases", Mode::Case, 7),
    ("graders", Mode::Grader, 12),
];

#[test]
fn every_documented_json_example_still_parses() {
    let mut checked: Vec<Mode> = Vec::new();

    for file in markdown_files() {
        for block in extract_blocks(&file) {
            if block.declared_mode == Some(Mode::Skip) {
                continue;
            }

            let value: Value = serde_json::from_str(&block.body).unwrap_or_else(|e| {
                panic!(
                    "{}: this example is not valid JSON: {e}\n\
                     If it is a deliberate shape rather than a document, open its fence with: \
                     ```json trg-example=skip\n---\n{}---",
                    block.where_(),
                    block.body
                )
            });

            let mode = block.declared_mode.unwrap_or_else(|| inferred_mode(&value));
            match mode {
                Mode::Suite => check_parses_as_suite(&block),
                Mode::Case => check_parses_as_case(&block),
                Mode::Grader => check_parses_as_grader(&block, &value),
                Mode::Syntax | Mode::Skip => {}
            }
            checked.push(mode);
        }
    }

    for (kind, mode, floor) in FLOORS {
        let found = checked.iter().filter(|m| **m == mode).count();
        assert!(
            found >= floor,
            "expected the docs to still hold at least {floor} {kind} examples, found {found}"
        );
    }
}
