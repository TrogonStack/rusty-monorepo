//! MCP mock declarations: a suite tells trg what a tool call should answer with instead of
//! reaching a real MCP server, so a case can exercise a skill's MCP-calling behaviour
//! without a network dependency or a live account.
//!
//! Only `type: fixed` mocks are implemented here. Record/replay and `type: agent` mocks are
//! a different feature with a different failure mode and are deliberately left unsupported,
//! with a named error rather than a silent fallback.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use gray_matter::{engine::YAML, Matter, Pod};
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::evals::EVAL_SUITE_DIR_NAME;
use super::hex_encode;

const MOCKS_DIR_NAME: &str = "mocks";

#[derive(Debug, thiserror::Error)]
pub enum MocksError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{path}: mock declaration has no frontmatter")]
    MissingFrontmatter { path: PathBuf },
    #[error("{path}: {detail}")]
    InvalidFrontmatter { path: PathBuf, detail: String },
    #[error("{path}: mock type `{found}` is not yet supported; only `fixed` mocks are implemented")]
    UnsupportedMockType { path: PathBuf, found: String },
    #[error("{path}: expect path `{expect_path}` has a constraint shape that is not supported")]
    UnsupportedConstraintShape { path: PathBuf, expect_path: String },
    #[error("{path}: `{{{{file:{reference}}}}}` does not resolve to a file")]
    FileReferenceNotFound { path: PathBuf, reference: String },
    #[error(transparent)]
    UnresolvedInputPlaceholder(#[from] UnresolvedInputPlaceholder),
}

/// The only way substituting `{{input....}}` can fail: the mock's text names a field the
/// call did not carry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{server}/{tool}: `{{{{input.{dotted_path}}}}}` has no matching value in the call input")]
pub struct UnresolvedInputPlaceholder {
    pub server: ServerName,
    pub tool: ToolName,
    pub dotted_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ServerName(String);

impl ServerName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ServerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ServerName {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ToolName(String);

impl ToolName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ToolName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ToolName {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MockType {
    Fixed,
}

impl MockType {
    fn parse(raw: &str, path: &Path) -> Result<Self, MocksError> {
        match raw {
            "fixed" => Ok(Self::Fixed),
            other => Err(MocksError::UnsupportedMockType {
                path: path.to_path_buf(),
                found: other.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JsonTypeName {
    String,
    Number,
    Boolean,
    Object,
    Array,
}

impl JsonTypeName {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "string" => Some(Self::String),
            "number" => Some(Self::Number),
            "boolean" => Some(Self::Boolean),
            "object" => Some(Self::Object),
            "array" => Some(Self::Array),
            _ => None,
        }
    }

    fn matches(self, value: &serde_json::Value) -> bool {
        match self {
            Self::String => value.is_string(),
            Self::Number => value.is_number(),
            Self::Boolean => value.is_boolean(),
            Self::Object => value.is_object(),
            Self::Array => value.is_array(),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Object => "object",
            Self::Array => "array",
        }
    }
}

/// A dotted path into a tool call's input, e.g. `repo.owner`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ExpectPath(String);

impl ExpectPath {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('.')
    }
}

impl From<&str> for ExpectPath {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExpectConstraint {
    Regex(String),
    Literal(String),
    OneOf(Vec<String>),
    TypeName(JsonTypeName),
}

impl ExpectConstraint {
    fn from_pod(pod: &Pod, path: &Path, expect_path: &str) -> Result<Self, MocksError> {
        match pod {
            Pod::String(raw) => Ok(Self::from_string_value(raw)),
            Pod::Array(items) => {
                let mut literals = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        Pod::String(s) => literals.push(s.clone()),
                        _ => {
                            return Err(MocksError::UnsupportedConstraintShape {
                                path: path.to_path_buf(),
                                expect_path: expect_path.to_string(),
                            })
                        }
                    }
                }
                Ok(Self::OneOf(literals))
            }
            _ => Err(MocksError::UnsupportedConstraintShape {
                path: path.to_path_buf(),
                expect_path: expect_path.to_string(),
            }),
        }
    }

    fn from_string_value(raw: &str) -> Self {
        if raw.len() >= 2 && raw.starts_with('/') && raw.ends_with('/') {
            return Self::Regex(raw[1..raw.len() - 1].to_string());
        }
        if let Some(type_name) = JsonTypeName::parse(raw) {
            return Self::TypeName(type_name);
        }
        Self::Literal(raw.to_string())
    }

    fn describe(&self) -> String {
        match self {
            Self::Regex(pattern) => format!("/{pattern}/"),
            Self::Literal(value) => value.clone(),
            Self::OneOf(values) => values.join(", "),
            Self::TypeName(type_name) => type_name.label().to_string(),
        }
    }

    fn matches(&self, value: &serde_json::Value) -> bool {
        match self {
            Self::Literal(expected) => value_as_text(value).is_some_and(|actual| actual == *expected),
            Self::OneOf(options) => value_as_text(value).is_some_and(|actual| options.contains(&actual)),
            Self::TypeName(type_name) => type_name.matches(value),
            Self::Regex(pattern) => match Regex::new(pattern) {
                Ok(re) => value_as_text(value).is_some_and(|actual| re.is_match(&actual)),
                Err(_) => false,
            },
        }
    }
}

fn value_as_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MockDeclaration {
    pub mock_type: MockType,
    #[serde(default)]
    pub expect: BTreeMap<ExpectPath, ExpectConstraint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub body: String,
}

impl MockDeclaration {
    /// Every violation the call's input has against this declaration's `expect` map. An
    /// unmatched path (the input never carries it) is a violation with the received value
    /// reported as `null`, since "absent" and "wrong" are both the mock refusing what it
    /// was told to expect.
    pub fn check_expectations(
        &self,
        server: &ServerName,
        tool: &ToolName,
        input: &serde_json::Value,
    ) -> Vec<MockViolation> {
        let mut violations = Vec::new();
        for (path, constraint) in &self.expect {
            let received = lookup_path(input, path);
            let value = received.unwrap_or(&serde_json::Value::Null);
            if !constraint.matches(value) {
                violations.push(MockViolation {
                    server: server.clone(),
                    tool: tool.clone(),
                    path: path.clone(),
                    constraint: constraint.describe(),
                    received: value.clone(),
                });
            }
        }
        violations
    }
}

fn lookup_path<'a>(value: &'a serde_json::Value, path: &ExpectPath) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for segment in path.segments() {
        current = current.as_object()?.get(segment)?;
    }
    Some(current)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MockViolation {
    pub server: ServerName,
    pub tool: ToolName,
    pub path: ExpectPath,
    pub constraint: String,
    pub received: serde_json::Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MockSet {
    pub servers: BTreeMap<ServerName, BTreeMap<ToolName, MockDeclaration>>,
}

impl MockSet {
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    pub fn server_names(&self) -> impl Iterator<Item = &ServerName> {
        self.servers.keys()
    }

    pub fn tools_for(&self, server: &ServerName) -> Option<&BTreeMap<ToolName, MockDeclaration>> {
        self.servers.get(server)
    }

    /// A stable digest of the fully resolved set: after suite/per-case override merging and
    /// after `{{file:...}}` substitution. Hashing the resolved structure, rather than raw
    /// file bytes, means a file a mock references by `{{file:...}}` also participates: the
    /// mock's own text did not change, but the answer a run gets from it did.
    pub fn content_hash(&self) -> String {
        let json = serde_json::to_string(self).expect("MockSet serializes");
        let mut hasher = Sha256::new();
        hasher.update(json.as_bytes());
        format!("sha256:{}", hex_encode(hasher.finalize()))
    }
}

/// Resolve the mock set declared for an eval case: suite-level mocks at
/// `evals/mocks/<server>/<tool>.md`, overridden file-by-file by any case-scoped mocks at
/// `evals/<eval_id>/mocks/<server>/<tool>.md`.
///
/// The spec this shipped from named the per-case path `evals/cases/<id>/mocks/...`, but no
/// `cases/` segment exists anywhere fixtures are resolved from (see `compute_fixture_hash`),
/// so the per-case path here matches how `EvalCase` files are actually resolved instead.
pub fn resolve_mock_set(skill_path: &Path, eval_id: &str) -> Result<MockSet, MocksError> {
    let suite_dir = skill_path.join(EVAL_SUITE_DIR_NAME).join(MOCKS_DIR_NAME);
    let case_dir = skill_path.join(EVAL_SUITE_DIR_NAME).join(eval_id).join(MOCKS_DIR_NAME);

    let mut set = MockSet::default();
    merge_mock_dir(&suite_dir, &mut set)?;
    merge_mock_dir(&case_dir, &mut set)?;
    Ok(set)
}

/// The name `mock-calls.jsonl` is written under, relative to a run's own directory.
pub const MOCK_CALLS_LOG_NAME: &str = "mock-calls.jsonl";
const MATERIALIZED_MOCKS_DIR_NAME: &str = "mcp-mocks";
const MCP_CONFIG_FILE_NAME: &str = "mcp-config.json";

/// Materialize a resolved mock set for a run: one JSON file per declared tool, plus an
/// `--mcp-config` document that points each declared server at its own invocation of the
/// hidden `mock-server` subcommand. Returns the path to that config document.
///
/// The subcommand is handed only the JSON this function writes; it never re-parses suite
/// markdown or re-applies a per-case override itself. That keeps the mock server process
/// unable to disagree with the very `MockSet` a run's cache key was computed from, since
/// both are reading the same already-resolved declarations rather than deriving them twice.
pub fn materialize_mock_set(mock_set: &MockSet, run_dir: &Path, trg_binary: &Path) -> Result<PathBuf, MocksError> {
    let mocks_dir = run_dir.join(MATERIALIZED_MOCKS_DIR_NAME);
    let calls_path = run_dir.join(MOCK_CALLS_LOG_NAME);

    let mut mcp_servers = serde_json::Map::new();
    for (server, tools) in &mock_set.servers {
        let server_dir = mocks_dir.join(server.as_str());
        create_dir(&server_dir)?;
        for (tool, declaration) in tools {
            let tool_path = server_dir.join(format!("{}.json", tool.as_str()));
            let json = serde_json::to_string_pretty(declaration).expect("MockDeclaration serializes");
            write_file(&tool_path, &json)?;
        }

        mcp_servers.insert(
            server.to_string(),
            serde_json::json!({
                "command": trg_binary.to_string_lossy(),
                "args": [
                    "ai",
                    "skills",
                    "eval",
                    "mock-server",
                    "--mocks",
                    server_dir.to_string_lossy(),
                    "--server",
                    server.as_str(),
                    "--calls",
                    calls_path.to_string_lossy(),
                ],
            }),
        );
    }

    // Created empty up front, not only appended to on the first call: a run that never
    // calls a declared tool must still produce `mock-calls.jsonl` as an artifact, so a
    // reader can tell "declared but unused" apart from "the mock server never started".
    if !calls_path.is_file() {
        write_file(&calls_path, "")?;
    }

    let config = serde_json::json!({ "mcpServers": mcp_servers });
    let config_path = run_dir.join(MCP_CONFIG_FILE_NAME);
    let config_json = serde_json::to_string_pretty(&config).expect("mcp config serializes");
    write_file(&config_path, &config_json)?;
    Ok(config_path)
}

fn create_dir(dir: &Path) -> Result<(), MocksError> {
    fs::create_dir_all(dir).map_err(|source| MocksError::Io {
        path: dir.to_path_buf(),
        source,
    })
}

fn write_file(path: &Path, content: &str) -> Result<(), MocksError> {
    fs::write(path, content).map_err(|source| MocksError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn merge_mock_dir(dir: &Path, set: &mut MockSet) -> Result<(), MocksError> {
    if !dir.is_dir() {
        return Ok(());
    }
    for server_entry in read_dir_sorted(dir)? {
        if !server_entry.is_dir() {
            continue;
        }
        let server = ServerName::from(file_name_str(&server_entry).as_str());
        for tool_entry in read_dir_sorted(&server_entry)? {
            if tool_entry.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let tool_name = tool_entry.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
            let tool = ToolName::from(tool_name);
            let declaration = parse_mock_file(&tool_entry, dir)?;
            set.servers.entry(server.clone()).or_default().insert(tool, declaration);
        }
    }
    Ok(())
}

fn read_dir_sorted(dir: &Path) -> Result<Vec<PathBuf>, MocksError> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|source| MocksError::Io {
            path: dir.to_path_buf(),
            source,
        })?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    Ok(entries)
}

fn file_name_str(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string()
}

/// Parse a single mock declaration file and eagerly resolve any `{{file:<relative path>}}`
/// references against `mocks_root` (the `mocks/` directory the file was found under).
///
/// `{{file:...}}` never depends on a call's arguments, so resolving it here, at case
/// resolution time, means a missing referenced file is a loud failure before any subprocess
/// is spawned, rather than something a running mock server would have to explain. `{{input.
/// ...}}` placeholders are left untouched: they can only be resolved once an actual call
/// arrives, so that substitution happens inside the mock server process instead.
fn parse_mock_file(path: &Path, mocks_root: &Path) -> Result<MockDeclaration, MocksError> {
    let content = fs::read_to_string(path).map_err(|source| MocksError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let matter = Matter::<YAML>::new();
    let parsed = matter.parse(&content).map_err(|e| MocksError::InvalidFrontmatter {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;
    let data: Pod = parsed.data.ok_or_else(|| MocksError::MissingFrontmatter {
        path: path.to_path_buf(),
    })?;
    let fields = data.as_hashmap().map_err(|e| MocksError::InvalidFrontmatter {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;

    let type_raw = fields
        .get("type")
        .and_then(|pod| pod.as_string().ok())
        .unwrap_or_else(|| "fixed".to_string());
    let mock_type = MockType::parse(&type_raw, path)?;

    let mut expect = BTreeMap::new();
    if let Some(expect_pod) = fields.get("expect") {
        let expect_map = expect_pod
            .as_hashmap()
            .map_err(|_| MocksError::UnsupportedConstraintShape {
                path: path.to_path_buf(),
                expect_path: "expect".to_string(),
            })?;
        for (key, value_pod) in expect_map {
            let constraint = ExpectConstraint::from_pod(&value_pod, path, &key)?;
            expect.insert(ExpectPath(key), constraint);
        }
    }

    let error = fields.get("error").and_then(|pod| pod.as_string().ok());

    let body = resolve_file_references(&parsed.content, path, mocks_root)?;

    Ok(MockDeclaration {
        mock_type,
        expect,
        error,
        body,
    })
}

fn resolve_file_references(body: &str, mock_file: &Path, mocks_root: &Path) -> Result<String, MocksError> {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find("{{file:") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "{{file:".len()..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let reference = after[..end].trim();
        let resolved_path = mocks_root.join(reference);
        let contents = fs::read_to_string(&resolved_path).map_err(|_| MocksError::FileReferenceNotFound {
            path: mock_file.to_path_buf(),
            reference: reference.to_string(),
        })?;
        out.push_str(&contents);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Substitute `{{input.<dotted.path>}}` placeholders against a call's actual arguments.
///
/// An unresolvable placeholder is a hard error rather than an empty string: a mock body that
/// silently blanked out a field the skill was meant to read back would be indistinguishable
/// from a skill bug, so the mock server refuses the call by name instead of guessing.
pub fn substitute_input_placeholders(
    body: &str,
    server: &ServerName,
    tool: &ToolName,
    input: &serde_json::Value,
) -> Result<String, UnresolvedInputPlaceholder> {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find("{{input.") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "{{input.".len()..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let dotted_path = after[..end].trim();
        let path = ExpectPath(dotted_path.to_string());
        let value = lookup_path(input, &path).ok_or_else(|| UnresolvedInputPlaceholder {
            server: server.clone(),
            tool: tool.clone(),
            dotted_path: dotted_path.to_string(),
        })?;
        out.push_str(&value_as_text(value).unwrap_or_else(|| value.to_string()));
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// What a `tools/call` against a fixed mock answers with, before it is wrapped in whatever
/// transport-level shape the caller needs (MCP `CallToolResult` for the mock server, a plain
/// value for a test).
#[derive(Debug, Clone, PartialEq)]
pub struct MockCallOutcome {
    pub violations: Vec<MockViolation>,
    pub answer: MockCallAnswer,
}

/// Whether the mock could answer the call at all. Kept beside the violations rather than in
/// an `Err` so that a call which both breaks an `expect` constraint and names a field it was
/// never given still reports the violation it earned.
#[derive(Debug, Clone, PartialEq)]
pub enum MockCallAnswer {
    Answered { text: String, is_error: bool },
    Unresolved(UnresolvedInputPlaceholder),
}

/// Resolve one `tools/call` against a fixed mock: check the call's arguments against
/// `expect`, then substitute `{{input....}}` into whichever of `body`/`error` answers the
/// call.
///
/// A violation does not stop the mock from answering as declared: the whole point of
/// recording it, rather than failing the call outright, is that a skill under test should
/// see the same tool response either way, and have the violation surface later as an
/// ordinary failing assertion instead of an inline protocol error a skill has to somehow
/// handle. An unresolved `{{input....}}` placeholder is a different kind of problem, a
/// mismatch between the mock's own text and the call it was written for, and that does stop
/// the call.
pub fn resolve_call(
    declaration: &MockDeclaration,
    server: &ServerName,
    tool: &ToolName,
    input: &serde_json::Value,
) -> MockCallOutcome {
    let violations = declaration.check_expectations(server, tool, input);
    let (raw, is_error) = match &declaration.error {
        Some(error_text) => (error_text.as_str(), true),
        None => (declaration.body.as_str(), false),
    };
    let answer = match substitute_input_placeholders(raw, server, tool, input) {
        Ok(text) => MockCallAnswer::Answered { text, is_error },
        Err(unresolved) => MockCallAnswer::Unresolved(unresolved),
    };
    MockCallOutcome { violations, answer }
}

/// One `tools/call` as the mock server logged it: what it was asked, and what it found
/// wrong with the call, if anything.
///
/// Written for every call, not only violating ones, because `mock-calls.jsonl` existing is
/// itself part of the contract (a later feature grades against it); an empty `violations`
/// list is the common case, not something worth pruning from the log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MockCallLogEntry {
    pub server: ServerName,
    pub tool: ToolName,
    pub input: serde_json::Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<MockViolation>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_mock(dir: &Path, server: &str, tool: &str, content: &str) {
        let server_dir = dir.join(server);
        fs::create_dir_all(&server_dir).unwrap();
        fs::write(server_dir.join(format!("{tool}.md")), content).unwrap();
    }

    #[test]
    fn a_suite_with_no_mocks_directory_resolves_to_an_empty_set() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        fs::create_dir_all(skill.join("evals/one")).unwrap();
        let set = resolve_mock_set(&skill, "one").unwrap();
        assert!(set.is_empty());
    }

    #[test]
    fn fixed_mock_parses_expect_and_body() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        let suite_mocks = skill.join("evals/mocks");
        write_mock(
            &suite_mocks,
            "github",
            "create_issue",
            "---\ntype: fixed\nexpect:\n  repo: /^acme\\//\n  title: string\n---\n{\"id\": 1}",
        );
        fs::create_dir_all(skill.join("evals/one")).unwrap();

        let set = resolve_mock_set(&skill, "one").unwrap();
        let server = ServerName::from("github");
        let tool = ToolName::from("create_issue");
        let declaration = set.tools_for(&server).unwrap().get(&tool).unwrap();
        assert_eq!(declaration.mock_type, MockType::Fixed);
        assert_eq!(declaration.body, "{\"id\": 1}");
        assert_eq!(
            declaration.expect.get(&ExpectPath("repo".to_string())),
            Some(&ExpectConstraint::Regex("^acme\\/".to_string()))
        );
        assert_eq!(
            declaration.expect.get(&ExpectPath("title".to_string())),
            Some(&ExpectConstraint::TypeName(JsonTypeName::String))
        );
    }

    #[test]
    fn agent_mock_type_is_rejected_by_name() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        let suite_mocks = skill.join("evals/mocks");
        write_mock(&suite_mocks, "github", "create_issue", "---\ntype: agent\n---\nbody");
        fs::create_dir_all(skill.join("evals/one")).unwrap();

        let err = resolve_mock_set(&skill, "one").unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("agent"),
            "error should name the rejected type: {message}"
        );
        assert!(message.contains("not yet supported"));
    }

    #[test]
    fn per_case_mock_overrides_suite_mock_file_by_file() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        write_mock(
            &skill.join("evals/mocks"),
            "github",
            "create_issue",
            "---\ntype: fixed\n---\nsuite body",
        );
        write_mock(
            &skill.join("evals/mocks"),
            "github",
            "close_issue",
            "---\ntype: fixed\n---\nsuite close",
        );
        write_mock(
            &skill.join("evals/one/mocks"),
            "github",
            "create_issue",
            "---\ntype: fixed\n---\ncase body",
        );

        let set = resolve_mock_set(&skill, "one").unwrap();
        let server = ServerName::from("github");
        let tools = set.tools_for(&server).unwrap();
        assert_eq!(tools.get(&ToolName::from("create_issue")).unwrap().body, "case body");
        assert_eq!(tools.get(&ToolName::from("close_issue")).unwrap().body, "suite close");
    }

    #[test]
    fn file_reference_is_resolved_eagerly_relative_to_mocks_root() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        let suite_mocks = skill.join("evals/mocks");
        fs::create_dir_all(&suite_mocks).unwrap();
        fs::write(suite_mocks.join("payload.json"), "{\"ok\":true}").unwrap();
        write_mock(
            &suite_mocks,
            "github",
            "create_issue",
            "---\ntype: fixed\n---\n{{file:payload.json}}",
        );
        fs::create_dir_all(skill.join("evals/one")).unwrap();

        let set = resolve_mock_set(&skill, "one").unwrap();
        let declaration = set
            .tools_for(&ServerName::from("github"))
            .unwrap()
            .get(&ToolName::from("create_issue"))
            .unwrap();
        assert_eq!(declaration.body, "{\"ok\":true}");
    }

    #[test]
    fn missing_file_reference_is_a_hard_error_not_an_empty_string() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        let suite_mocks = skill.join("evals/mocks");
        write_mock(
            &suite_mocks,
            "github",
            "create_issue",
            "---\ntype: fixed\n---\n{{file:missing.json}}",
        );
        fs::create_dir_all(skill.join("evals/one")).unwrap();

        let err = resolve_mock_set(&skill, "one").unwrap_err();
        assert!(matches!(err, MocksError::FileReferenceNotFound { .. }));
    }

    #[test]
    fn content_hash_changes_when_a_mock_body_changes() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        write_mock(
            &skill.join("evals/mocks"),
            "github",
            "create_issue",
            "---\ntype: fixed\n---\nfirst",
        );
        fs::create_dir_all(skill.join("evals/one")).unwrap();
        let first = resolve_mock_set(&skill, "one").unwrap().content_hash();

        write_mock(
            &skill.join("evals/mocks"),
            "github",
            "create_issue",
            "---\ntype: fixed\n---\nsecond",
        );
        let second = resolve_mock_set(&skill, "one").unwrap().content_hash();

        assert_ne!(first, second);
    }

    #[test]
    fn content_hash_changes_when_a_referenced_file_changes() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        let suite_mocks = skill.join("evals/mocks");
        fs::create_dir_all(&suite_mocks).unwrap();
        fs::write(suite_mocks.join("payload.json"), "one").unwrap();
        write_mock(
            &suite_mocks,
            "github",
            "create_issue",
            "---\ntype: fixed\n---\n{{file:payload.json}}",
        );
        fs::create_dir_all(skill.join("evals/one")).unwrap();

        let first = resolve_mock_set(&skill, "one").unwrap().content_hash();
        fs::write(suite_mocks.join("payload.json"), "two").unwrap();
        let second = resolve_mock_set(&skill, "one").unwrap().content_hash();

        assert_ne!(
            first, second,
            "a file a mock references by {{file:...}} is a run input just as much as the mock text itself"
        );
    }

    #[test]
    fn expect_violation_reports_path_constraint_and_received_value() {
        let declaration = MockDeclaration {
            mock_type: MockType::Fixed,
            expect: BTreeMap::from([(
                ExpectPath("repo".to_string()),
                ExpectConstraint::Regex("^acme/".to_string()),
            )]),
            error: None,
            body: "{}".to_string(),
        };
        let server = ServerName::from("github");
        let tool = ToolName::from("create_issue");
        let input = serde_json::json!({"repo": "other/repo"});

        let violations = declaration.check_expectations(&server, &tool, &input);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].path.as_str(), "repo");
        assert_eq!(violations[0].received, serde_json::json!("other/repo"));
    }

    #[test]
    fn expect_satisfied_produces_no_violation() {
        let declaration = MockDeclaration {
            mock_type: MockType::Fixed,
            expect: BTreeMap::from([(
                ExpectPath("repo".to_string()),
                ExpectConstraint::Regex("^acme/".to_string()),
            )]),
            error: None,
            body: "{}".to_string(),
        };
        let server = ServerName::from("github");
        let tool = ToolName::from("create_issue");
        let input = serde_json::json!({"repo": "acme/widgets"});

        assert!(declaration.check_expectations(&server, &tool, &input).is_empty());
    }

    #[test]
    fn a_mock_type_reads_back_from_json_as_the_word_its_frontmatter_spells() {
        assert_eq!(
            serde_json::to_value(MockType::Fixed).unwrap(),
            serde_json::json!("fixed")
        );
        assert_eq!(
            serde_json::from_value::<MockType>(serde_json::json!("fixed")).unwrap(),
            MockType::Fixed,
            "the frontmatter parser and the resolved-json reader answer to one vocabulary"
        );
    }

    #[test]
    fn every_json_type_name_reads_back_from_json_as_the_word_expect_spells() {
        for name in [
            JsonTypeName::String,
            JsonTypeName::Number,
            JsonTypeName::Boolean,
            JsonTypeName::Object,
            JsonTypeName::Array,
        ] {
            assert_eq!(
                serde_json::to_value(name).unwrap(),
                serde_json::json!(name.label()),
                "{name:?}"
            );
            assert_eq!(JsonTypeName::parse(name.label()), Some(name), "{name:?}");
        }
    }

    #[test]
    fn input_placeholder_substitutes_from_call_arguments() {
        let server = ServerName::from("github");
        let tool = ToolName::from("create_issue");
        let input = serde_json::json!({"repo": "acme/widgets"});
        let out = substitute_input_placeholders("repo is {{input.repo}}", &server, &tool, &input).unwrap();
        assert_eq!(out, "repo is acme/widgets");
    }

    #[test]
    fn unresolved_input_placeholder_is_a_call_time_error_not_an_empty_string() {
        let server = ServerName::from("github");
        let tool = ToolName::from("create_issue");
        let input = serde_json::json!({});
        let err = substitute_input_placeholders("repo is {{input.repo}}", &server, &tool, &input).unwrap_err();
        assert_eq!(err.dotted_path, "repo");
    }

    #[test]
    fn resolve_call_answers_body_and_still_reports_a_violation() {
        let declaration = MockDeclaration {
            mock_type: MockType::Fixed,
            expect: BTreeMap::from([(
                ExpectPath("repo".to_string()),
                ExpectConstraint::Regex("^acme/".to_string()),
            )]),
            error: None,
            body: "{{input.repo}} created".to_string(),
        };
        let server = ServerName::from("github");
        let tool = ToolName::from("create_issue");
        let input = serde_json::json!({"repo": "other/repo"});

        let outcome = resolve_call(&declaration, &server, &tool, &input);
        assert_eq!(
            outcome.answer,
            MockCallAnswer::Answered {
                text: "other/repo created".to_string(),
                is_error: false
            }
        );
        assert_eq!(
            outcome.violations.len(),
            1,
            "the call still answers per its declaration even though the input violated expect"
        );
    }

    #[test]
    fn resolve_call_with_a_declared_error_answers_as_a_tool_error() {
        let declaration = MockDeclaration {
            mock_type: MockType::Fixed,
            expect: BTreeMap::new(),
            error: Some("rate limited".to_string()),
            body: "unused".to_string(),
        };
        let server = ServerName::from("github");
        let tool = ToolName::from("create_issue");
        let input = serde_json::json!({});

        let outcome = resolve_call(&declaration, &server, &tool, &input);
        assert_eq!(
            outcome.answer,
            MockCallAnswer::Answered {
                text: "rate limited".to_string(),
                is_error: true
            }
        );
    }

    #[test]
    fn resolve_call_fails_the_call_on_an_unresolved_placeholder() {
        let declaration = MockDeclaration {
            mock_type: MockType::Fixed,
            expect: BTreeMap::new(),
            error: None,
            body: "{{input.missing}}".to_string(),
        };
        let server = ServerName::from("github");
        let tool = ToolName::from("create_issue");
        let input = serde_json::json!({});

        let outcome = resolve_call(&declaration, &server, &tool, &input);
        assert!(matches!(outcome.answer, MockCallAnswer::Unresolved(_)));
    }

    #[test]
    fn resolve_call_still_reports_a_violation_when_substitution_fails() {
        let declaration = MockDeclaration {
            mock_type: MockType::Fixed,
            expect: BTreeMap::from([(
                ExpectPath("repo".to_string()),
                ExpectConstraint::Regex("^acme/".to_string()),
            )]),
            error: None,
            body: "{{input.missing}}".to_string(),
        };
        let server = ServerName::from("github");
        let tool = ToolName::from("create_issue");
        let input = serde_json::json!({"repo": "other/repo"});

        let outcome = resolve_call(&declaration, &server, &tool, &input);

        assert!(matches!(outcome.answer, MockCallAnswer::Unresolved(_)));
        assert_eq!(
            outcome.violations.len(),
            1,
            "a call that breaks expect and then fails substitution still earned the violation"
        );
    }

    #[test]
    fn materialize_writes_one_json_file_per_tool_and_an_mcp_config_naming_the_mock_server_subcommand() {
        let temp = tempdir().unwrap();
        let skill = temp.path().join("skill");
        write_mock(
            &skill.join("evals/mocks"),
            "github",
            "create_issue",
            "---\ntype: fixed\n---\ncreated",
        );
        let mock_set = resolve_mock_set(&skill, "one").unwrap();
        let run_dir = temp.path().join("run-dir");
        let trg_binary = Path::new("/usr/local/bin/trg");

        let config_path = materialize_mock_set(&mock_set, &run_dir, trg_binary).unwrap();

        let tool_json_path = run_dir.join("mcp-mocks/github/create_issue.json");
        assert!(tool_json_path.is_file());
        let declaration: MockDeclaration = serde_json::from_str(&fs::read_to_string(&tool_json_path).unwrap()).unwrap();
        assert_eq!(declaration.body, "created");

        let config: serde_json::Value = serde_json::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
        let github_server = &config["mcpServers"]["github"];
        assert_eq!(github_server["command"], "/usr/local/bin/trg");
        let args: Vec<String> = github_server["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            args,
            vec![
                "ai",
                "skills",
                "eval",
                "mock-server",
                "--mocks",
                run_dir.join("mcp-mocks/github").to_string_lossy().as_ref(),
                "--server",
                "github",
                "--calls",
                run_dir.join(MOCK_CALLS_LOG_NAME).to_string_lossy().as_ref(),
            ]
        );
    }
}
