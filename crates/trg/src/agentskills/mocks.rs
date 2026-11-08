//! MCP mock declarations: a suite tells trg what a tool call should answer with instead of
//! reaching a real MCP server, so a case can exercise a skill's MCP-calling behaviour
//! without a network dependency or a live account.
//!
//! Only `type: fixed` mocks are implemented here. Record/replay and `type: agent` mocks are
//! a different feature with a different failure mode and are deliberately left unsupported,
//! with a named error rather than a silent fallback.

use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
mod constants;
pub use constants::MOCK_CALLS_LOG_NAME;
use constants::{
    MATERIALIZED_MOCKS_DIR_NAME, MCP_CONFIG_FILE_NAME, MCP_CONFIG_TOML_FILE_NAME, MCP_LAUNCH_DIR_NAME, MOCKS_DIR_NAME,
};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gray_matter::engine::YAML;
use gray_matter::{Matter, Pod};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::evals::EvalDirName;
use super::graders::RegexPattern;
use super::hex_encode;
use super::redact::is_secret_env_key;
use super::runner::forwarding::is_exporter_header_var;
use super::runner::TelemetryForwarding;

#[derive(Debug, thiserror::Error)]
pub enum MocksError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{path}: mock declaration has no frontmatter")]
    MissingFrontmatter { path: PathBuf },
    #[error("{path}: {source}")]
    InvalidFrontmatter { path: PathBuf, source: gray_matter::Error },
    #[error("{path}: mock type `{found}` is not yet supported; only `fixed` mocks are implemented")]
    UnsupportedMockType { path: PathBuf, found: String },
    #[error("{path}: expect path `{expect_path}` has a constraint shape that is not supported")]
    UnsupportedConstraintShape {
        path: PathBuf,
        expect_path: String,
        source: Option<gray_matter::Error>,
    },
    #[error("{path}: expect path `{expect_path}` declares `/{pattern}/`, which is not a valid regex: {source}")]
    InvalidExpectRegex {
        path: PathBuf,
        expect_path: String,
        pattern: String,
        source: regex::Error,
    },
    #[error("the mock server must be spawned by an absolute path, but `{path}` is relative")]
    RelativeMockServerBinary { path: PathBuf },
    #[error("the materialized mcp config cannot be handed to a harness: {detail}")]
    UnmaterializableConfig { detail: String },
    #[error("could not locate the running trg binary: {source}")]
    UnlocatableBinary { source: std::io::Error },
    #[error("{path}: `{{{{file:{reference}}}}}` does not resolve to a file")]
    FileReferenceNotFound {
        path: PathBuf,
        reference: String,
        source: std::io::Error,
    },
    #[error(transparent)]
    UnresolvedInputPlaceholder(#[from] UnresolvedInputPlaceholderError),
}

/// The only way substituting `{{input....}}` can fail: the mock's text names a field the
/// call did not carry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{server}/{tool}: `{{{{input.{dotted_path}}}}}` has no matching value in the call input")]
pub struct UnresolvedInputPlaceholderError {
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

/// Every variant is a struct variant, even the ones with a single field, so that an
/// internally tagged encoding always has a map to attach `kind` to. A newtype variant
/// wrapping a bare string or array cannot be represented that way; serde only discovers
/// that at serialization time, so the invalid shape does not fail to compile, it panics
/// the first time a mock declaring `expect` reaches `content_hash`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExpectConstraint {
    Regex { pattern: RegexPattern },
    Literal { value: String },
    OneOf { values: Vec<String> },
    TypeName { type_name: JsonTypeName },
}

impl ExpectConstraint {
    fn from_pod(pod: &Pod, path: &Path, expect_path: &str) -> Result<Self, MocksError> {
        match pod {
            Pod::String(raw) => Self::from_string_value(raw, path, expect_path),
            Pod::Array(items) => {
                let mut literals = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        Pod::String(s) => literals.push(s.clone()),
                        _ => {
                            return Err(MocksError::UnsupportedConstraintShape {
                                path: path.to_path_buf(),
                                expect_path: expect_path.to_string(),
                                source: None,
                            })
                        }
                    }
                }
                Ok(Self::OneOf { values: literals })
            }
            _ => Err(MocksError::UnsupportedConstraintShape {
                path: path.to_path_buf(),
                expect_path: expect_path.to_string(),
                source: None,
            }),
        }
    }

    /// A `/.../` constraint is compiled here, where the file that declares it is still in
    /// hand, rather than where it is matched. A pattern that fails to compile at match time
    /// is indistinguishable from one that simply did not match: the case reports a violation
    /// naming the call, and the typo in the mock never surfaces at all.
    fn from_string_value(raw: &str, path: &Path, expect_path: &str) -> Result<Self, MocksError> {
        if raw.len() >= 2 && raw.starts_with('/') && raw.ends_with('/') {
            let pattern = &raw[1..raw.len() - 1];
            return match RegexPattern::parse(pattern) {
                Ok(pattern) => Ok(Self::Regex { pattern }),
                Err(source) => Err(MocksError::InvalidExpectRegex {
                    path: path.to_path_buf(),
                    expect_path: expect_path.to_string(),
                    pattern: pattern.to_string(),
                    source,
                }),
            };
        }
        if let Some(type_name) = JsonTypeName::parse(raw) {
            return Ok(Self::TypeName { type_name });
        }
        Ok(Self::Literal { value: raw.to_string() })
    }

    fn describe(&self) -> String {
        match self {
            Self::Regex { pattern } => format!("/{pattern}/"),
            Self::Literal { value } => value.clone(),
            Self::OneOf { values } => values.join(", "),
            Self::TypeName { type_name } => type_name.label().to_string(),
        }
    }

    fn matches(&self, value: &serde_json::Value) -> bool {
        match self {
            Self::Literal { value: expected } => value_as_text(value).is_some_and(|actual| actual == *expected),
            Self::OneOf { values: options } => value_as_text(value).is_some_and(|actual| options.contains(&actual)),
            Self::TypeName { type_name } => type_name.matches(value),
            Self::Regex { pattern } => {
                let compiled = pattern.compile();
                value_as_text(value).is_some_and(|actual| compiled.is_match(&actual))
            }
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
pub fn resolve_mock_set(skill_path: &Path, eval_dir: &EvalDirName, eval_id: &str) -> Result<MockSet, MocksError> {
    let suite_dir = skill_path.join(eval_dir.as_str()).join(MOCKS_DIR_NAME);
    let case_dir = skill_path.join(eval_dir.as_str()).join(eval_id).join(MOCKS_DIR_NAME);

    let mut set = MockSet::default();
    merge_mock_dir(&suite_dir, &mut set)?;
    merge_mock_dir(&case_dir, &mut set)?;
    Ok(set)
}

/// One mock server as a harness has to be told to start it: a command and its arguments.
///
/// The two harnesses that can be driven with mocks read that in two different file formats,
/// so the table is built once and rendered twice. Building it per format is how the JSON a
/// report keeps as the record of what was injected and the TOML a run actually injected
/// would be free to disagree.
#[derive(Debug, Clone, Serialize)]
struct MockServerInvocation {
    command: String,
    args: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
}

/// Variables written into each mock server's `env` block, so a harness that starts the
/// server with a cleared environment still hands it the trace it belongs to.
///
/// Empty unless a trace is active, which keeps the generated config byte for byte what it
/// was before tracing existed. The exporter settings that let a mock server reach the
/// collector are added only when the operator forwards telemetry, the same switch that
/// lets the harness reach it. Exporter headers and other credential-shaped variables (an
/// mTLS `CLIENT_KEY`, for instance) routinely carry secrets, so they are held apart and
/// only ever reach the owner-only config a harness is handed, never the record a report
/// keeps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MockServerEnv {
    recorded: BTreeMap<String, String>,
    credentials: BTreeMap<String, String>,
}

impl MockServerEnv {
    pub fn none() -> Self {
        Self::default()
    }

    /// The current span's trace context, plus this process's OTLP exporter settings when
    /// the operator forwards telemetry.
    pub fn from_current_trace(forwarding: TelemetryForwarding) -> Self {
        Self::from_parts(
            crate::telemetry::propagation::carrier_pairs(),
            std::env::vars(),
            forwarding,
        )
    }

    fn from_parts(
        carrier: Vec<(String, String)>,
        process_env: impl IntoIterator<Item = (String, String)>,
        forwarding: TelemetryForwarding,
    ) -> Self {
        if carrier.is_empty() {
            return Self::none();
        }
        let mut env = Self {
            recorded: carrier.into_iter().collect(),
            credentials: BTreeMap::new(),
        };
        if forwarding == TelemetryForwarding::Off {
            return env;
        }
        let selected_items = process_env.into_iter().filter(|(key, _)| key.starts_with("OTEL_"));
        for (key, value) in selected_items {
            if is_exporter_header_var(&key) || is_secret_env_key(&key) {
                env.credentials.insert(key, value);
            } else if !key.ends_with("HEADERS") {
                env.recorded.entry(key).or_insert(value);
            }
        }
        env
    }

    pub fn is_empty(&self) -> bool {
        self.recorded.is_empty() && self.credentials.is_empty()
    }

    fn handed(&self) -> BTreeMap<String, String> {
        let mut env = self.recorded.clone();
        env.extend(self.credentials.clone());
        env
    }
}

#[derive(Debug, Clone, Serialize)]
struct CodexMcpServers {
    mcp_servers: BTreeMap<String, MockServerInvocation>,
}

/// Where a run's materialized mock server table was written, in each of the formats a
/// harness reads it from.
///
/// The two files always describe the same servers, so they are made together and travel
/// together: a caller holding only one of them cannot pick the wrong one for its harness,
/// and cannot reconstruct the other by guessing at a file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedMcpConfig {
    json: PathBuf,
    toml: PathBuf,
}

impl MaterializedMcpConfig {
    /// Both paths are resolved by a child process with a working directory of its own, so
    /// a relative one names a different file there than it does here.
    pub fn parse(json: PathBuf, toml: PathBuf) -> Result<Self, String> {
        for path in [&json, &toml] {
            if !path.is_absolute() {
                return Err(format!(
                    "an mcp config is read by a child process, so `{}` must be absolute",
                    path.display()
                ));
            }
        }
        Ok(Self { json, toml })
    }

    /// The `{"mcpServers": ...}` document claude-code reads through `--mcp-config`.
    pub fn json(&self) -> &Path {
        &self.json
    }

    /// The `[mcp_servers.*]` fragment codex reads as the `config.toml` of its config home.
    pub fn toml(&self) -> &Path {
        &self.toml
    }

    /// Remove the `mcp-launch` copy once the harness that was handed it has exited, whatever
    /// way it ended.
    ///
    /// A mock server reads its own environment, not this file: the harness reads it only to
    /// learn what to spawn, and does so once, at its own startup. Nothing still depends on it
    /// once the harness process is gone, so the credentials it carries do not need to survive
    /// into the report the run directory becomes.
    pub fn scrub(&self) -> io::Result<()> {
        let Some(dir) = self.json.parent() else {
            return Ok(());
        };
        match fs::remove_dir_all(dir) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(source),
        }
    }
}

/// Materialize a resolved mock set for a run: one JSON file per declared tool, plus the
/// mock server table in both of the formats a harness reads it from, each pointing every
/// declared server at its own invocation of the hidden `mock-server` subcommand.
///
/// The subcommand is handed only the JSON this function writes; it never re-parses suite
/// markdown or re-applies a per-case override itself. That keeps the mock server process
/// unable to disagree with the very `MockSet` a run's cache key was computed from, since
/// both are reading the same already-resolved declarations rather than deriving them twice.
///
/// The table is written twice: once beside the run as the record a report keeps, and once
/// under `mcp-launch` as the copy a harness is handed, the only one that carries exporter
/// headers.
pub fn materialize_mock_set(
    mock_set: &MockSet,
    run_dir: &Path,
    trg_binary: &MockServerBinary,
    server_env: &MockServerEnv,
) -> Result<MaterializedMcpConfig, MocksError> {
    // Every path written here is resolved by someone else: the harness reads the config
    // with its working directory set to the run's workspace, and it in turn spawns the
    // mock server. A relative `--out-dir` would have all of them resolve under the
    // workspace instead of the run directory, so the paths are pinned to a root once,
    // here, rather than at each of the places that read them back.
    let run_dir = &absolute_run_dir(run_dir)?;
    let mocks_dir = run_dir.join(MATERIALIZED_MOCKS_DIR_NAME);
    let calls_path = run_dir.join(MOCK_CALLS_LOG_NAME);

    let mut servers: BTreeMap<String, MockServerInvocation> = BTreeMap::new();
    for (server, tools) in &mock_set.servers {
        let server_dir = mocks_dir.join(server.as_str());
        create_dir(&server_dir)?;
        for (tool, declaration) in tools {
            let tool_path = server_dir.join(format!("{}.json", tool.as_str()));
            let json = serde_json::to_string_pretty(declaration).expect("MockDeclaration serializes");
            write_file(&tool_path, &json)?;
        }

        servers.insert(
            server.to_string(),
            MockServerInvocation {
                command: trg_binary.as_path().to_string_lossy().into_owned(),
                args: vec![
                    "ai".to_string(),
                    "skills".to_string(),
                    "eval".to_string(),
                    "mock-server".to_string(),
                    "--mocks-dir".to_string(),
                    server_dir.to_string_lossy().into_owned(),
                    "--server".to_string(),
                    server.to_string(),
                    "--calls-file".to_string(),
                    calls_path.to_string_lossy().into_owned(),
                ],
                env: BTreeMap::new(),
            },
        );
    }

    write_mcp_config(run_dir, &servers, &server_env.recorded, write_file)?;
    let launch_dir = run_dir.join(MCP_LAUNCH_DIR_NAME);
    create_dir(&launch_dir)?;
    let (json_path, toml_path) = write_mcp_config(&launch_dir, &servers, &server_env.handed(), write_owner_only_file)?;

    MaterializedMcpConfig::parse(json_path, toml_path).map_err(|detail| MocksError::UnmaterializableConfig { detail })
}

/// The `trg` binary a mock server is spawned by, as a path that means the same thing from
/// any working directory.
///
/// The harness reads the generated `--mcp-config` with its working directory set to the
/// run's workspace and spawns the mock server from there, so a relative command would be
/// looked up under the workspace and never found. `current_exe` is allowed to hand back a
/// relative path, so the guarantee is taken here rather than hoped for at the one call site
/// that happens to have it today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockServerBinary(PathBuf);

impl MockServerBinary {
    /// The running `trg` binary, resolved through the filesystem so that neither a relative
    /// invocation nor a symlinked one reaches the config document.
    pub fn locate() -> Result<Self, MocksError> {
        let invoked = std::env::current_exe().map_err(|source| MocksError::UnlocatableBinary { source })?;
        let resolved = fs::canonicalize(invoked).map_err(|source| MocksError::UnlocatableBinary { source })?;
        Self::at(resolved)
    }

    pub fn at(path: impl Into<PathBuf>) -> Result<Self, MocksError> {
        let path = path.into();
        match path.is_absolute() {
            true => Ok(Self(path)),
            false => Err(MocksError::RelativeMockServerBinary { path }),
        }
    }

    fn as_path(&self) -> &Path {
        &self.0
    }
}

/// The run directory as a path that means the same thing from any working directory.
fn absolute_run_dir(run_dir: &Path) -> Result<PathBuf, MocksError> {
    create_dir(run_dir)?;
    fs::canonicalize(run_dir).map_err(|source| MocksError::Io {
        path: run_dir.to_path_buf(),
        source,
    })
}

fn create_dir(dir: &Path) -> Result<(), MocksError> {
    fs::create_dir_all(dir).map_err(|source| MocksError::Io {
        path: dir.to_path_buf(),
        source,
    })
}

/// Write the mock server table into `dir` in both formats, every server carrying `env`.
fn write_mcp_config(
    dir: &Path,
    servers: &BTreeMap<String, MockServerInvocation>,
    env: &BTreeMap<String, String>,
    write: fn(&Path, &str) -> Result<(), MocksError>,
) -> Result<(PathBuf, PathBuf), MocksError> {
    let servers: BTreeMap<String, MockServerInvocation> = servers
        .iter()
        .map(|(name, invocation)| {
            let mut invocation = invocation.clone();
            invocation.env = env.clone();
            (name.clone(), invocation)
        })
        .collect();

    let json_path = dir.join(MCP_CONFIG_FILE_NAME);
    #[derive(Serialize)]
    struct McpConfig<'a> {
        #[serde(rename = "mcpServers")]
        servers: &'a BTreeMap<String, MockServerInvocation>,
    }
    let config = McpConfig { servers: &servers };
    write(
        &json_path,
        &serde_json::to_string_pretty(&config).expect("mcp config serializes"),
    )?;

    let toml_path = dir.join(MCP_CONFIG_TOML_FILE_NAME);
    let codex_config = CodexMcpServers { mcp_servers: servers };
    write(
        &toml_path,
        &toml::to_string(&codex_config).expect("mcp config serializes as toml"),
    )?;
    Ok((json_path, toml_path))
}

/// The config a harness is handed can carry exporter credentials, so it is readable by its
/// owner alone, the same as the `[otel]` table forwarded to codex.
fn write_owner_only_file(path: &Path, content: &str) -> Result<(), MocksError> {
    let io_error = |source| MocksError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(io_error)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(io_error)?;
    file.write_all(content.as_bytes()).map_err(io_error)
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
            let declaration = parse_mock_file(&tool_entry)?;
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
/// references against the directory the file itself is in.
///
/// Resolved from there rather than from the `mocks/` root so that a payload can sit beside
/// the tool that answers with it and two servers can each ship their own `payload.json`. A
/// file genuinely shared between servers is still reachable, by naming it `../shared.json`.
///
/// `{{file:...}}` never depends on a call's arguments, so resolving it here, at case
/// resolution time, means a missing referenced file is a loud failure before any subprocess
/// is spawned, rather than something a running mock server would have to explain. `{{input.
/// ...}}` placeholders are left untouched: they can only be resolved once an actual call
/// arrives, so that substitution happens inside the mock server process instead.
fn parse_mock_file(path: &Path) -> Result<MockDeclaration, MocksError> {
    let content = fs::read_to_string(path).map_err(|source| MocksError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let matter = Matter::<YAML>::new();
    let parsed = matter
        .parse(&content)
        .map_err(|source| MocksError::InvalidFrontmatter {
            path: path.to_path_buf(),
            source,
        })?;
    let data: Pod = parsed.data.ok_or_else(|| MocksError::MissingFrontmatter {
        path: path.to_path_buf(),
    })?;
    let fields = data.as_hashmap().map_err(|source| MocksError::InvalidFrontmatter {
        path: path.to_path_buf(),
        source,
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
            .map_err(|source| MocksError::UnsupportedConstraintShape {
                path: path.to_path_buf(),
                expect_path: "expect".to_string(),
                source: Some(source),
            })?;
        for (key, value_pod) in expect_map {
            let constraint = ExpectConstraint::from_pod(&value_pod, path, &key)?;
            expect.insert(ExpectPath(key), constraint);
        }
    }

    let error = fields.get("error").and_then(|pod| pod.as_string().ok());

    let body = resolve_file_references(&parsed.content, path)?;

    Ok(MockDeclaration {
        mock_type,
        expect,
        error,
        body,
    })
}

fn resolve_file_references(body: &str, mock_file: &Path) -> Result<String, MocksError> {
    let base = mock_file
        .parent()
        .expect("a mock file found by reading a directory sits in one");
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
        let resolved_path = base.join(reference);
        let contents = fs::read_to_string(&resolved_path).map_err(|source| MocksError::FileReferenceNotFound {
            path: mock_file.to_path_buf(),
            reference: reference.to_string(),
            source,
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
) -> Result<String, UnresolvedInputPlaceholderError> {
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
        let value = lookup_path(input, &path).ok_or_else(|| UnresolvedInputPlaceholderError {
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
    Unresolved(UnresolvedInputPlaceholderError),
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
    /// When the mock server received the call. Absent on logs written before this field
    /// existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received_at: Option<MockCallTimestamp>,
}

/// The instant a mock server received a call, serialized as RFC 3339 in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema)]
#[schemars(with = "String", extend("format" = "date-time"))]
pub struct MockCallTimestamp(chrono::DateTime<chrono::Utc>);

impl MockCallTimestamp {
    pub fn now() -> Self {
        Self(chrono::SubsecRound::trunc_subsecs(chrono::Utc::now(), 6))
    }

    pub fn as_datetime(&self) -> chrono::DateTime<chrono::Utc> {
        self.0
    }
}

impl Serialize for MockCallTimestamp {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0.to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
    }
}

impl<'de> Deserialize<'de> for MockCallTimestamp {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        chrono::DateTime::parse_from_rfc3339(&raw)
            .map(|at| Self(at.with_timezone(&chrono::Utc)))
            .map_err(serde::de::Error::custom)
    }
}

/// Every `tools/call` a run made against its mocks, rendered one call per line as
/// `<server>.<tool> <input>` with the input as compact JSON.
///
/// Rendered rather than handed over as the raw log, because the log is a serde encoding:
/// a pattern written against it would be answering questions about field order and
/// whitespace as much as about the call the agent made. The rendering is the documented
/// surface a case writes its patterns against, so it stays stable while the log format is
/// free to grow fields.
///
/// `violations` are left out. A run already reports each one as its own failing assertion,
/// and repeating them here would let one mismatch fail a case twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedMockCalls(String);

impl RenderedMockCalls {
    /// Reads the call log a run left behind. `Ok(None)` only where no log exists at all,
    /// which is a run no mock server ever came up for rather than a run that called nothing.
    ///
    /// Every other error is handed back rather than folded into that answer. A log that is
    /// there but unreadable is a broken artifact, and reporting it as an absent one would
    /// say the harness never offered the control, which is a different claim and the one
    /// that quietly drops the check out of the score.
    pub fn read(path: &Path) -> io::Result<Option<Self>> {
        match fs::read_to_string(path) {
            Ok(contents) => Ok(Some(Self::render(&contents))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// A line the log cannot be read back from is kept and marked rather than dropped: two
    /// mock servers append to the same file, so an interleaved write is possible, and a
    /// call that silently disappears reads to a grader as a call the agent never made.
    fn render(contents: &str) -> Self {
        let lines: Vec<String> = contents
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| match serde_json::from_str::<MockCallLogEntry>(line) {
                Ok(entry) => format!(
                    "{}.{} {}",
                    entry.server,
                    entry.tool,
                    serde_json::to_string(&entry.input).unwrap_or_else(|_| "null".to_string())
                ),
                Err(_) => format!("<unparsed> {}", line.trim()),
            })
            .collect();
        Self(lines.join("\n"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_text(self) -> String {
        self.0
    }
}

#[cfg(test)]
#[path = "mocks_tests.rs"]
mod tests;
