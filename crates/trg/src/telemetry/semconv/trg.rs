//! Attribute names `trg` owns.
//!
//! Everything here is hand-written, unlike [`super::generated`], because it
//! names concepts specific to `trg` rather than an OTel semantic convention.
//! Kept in its own module so the two are never confused for one another.

/// Resource attribute: the subcommand path this process is running
/// (`mcp proxy`, `ai skills eval run`, ...). One `trg` process runs exactly
/// one command, so this is a resource fact, not a span attribute.
pub const COMMAND: &str = "trg.command";

/// The eval run's own identifier, attached to its `run` span.
pub const EVAL_RUN_ID: &str = "trg.eval.run.id";

/// The case under test.
pub const EVAL_CASE_ID: &str = "trg.eval.case.id";

/// Which split (train/test/...) the case belongs to.
pub const EVAL_CASE_SPLIT: &str = "trg.eval.case.split";

/// The scenario within the case.
pub const EVAL_SCENARIO: &str = "trg.eval.scenario";

/// The iteration number for a case/scenario pair.
pub const EVAL_ITERATION: &str = "trg.eval.iteration";

/// The tool grant the run executed under.
pub const EVAL_TOOL_GRANT: &str = "trg.eval.tool_grant";

/// Which `-j` lane a run executed on.
pub const EVAL_LANE: &str = "trg.eval.lane";

/// Whether a run's cache lookup hit.
pub const EVAL_CACHE_HIT: &str = "trg.eval.cache.hit";

/// The attempt number within a run.
pub const EVAL_ATTEMPT: &str = "trg.eval.attempt";

/// Cost in USD attributed to an attempt.
pub const EVAL_COST_USD: &str = "trg.eval.cost.usd";

/// Harness-reported duration in milliseconds, kept beside wall time so
/// harness startup overhead is visible.
pub const EVAL_HARNESS_DURATION_MS: &str = "trg.eval.harness.duration_ms";

/// Harness-reported time spent waiting on the model API, in milliseconds,
/// for harnesses that report it apart from their total.
pub const EVAL_HARNESS_API_DURATION_MS: &str = "trg.eval.harness.api_duration_ms";

/// The grader's name, on a `grade` span's children.
pub const EVAL_GRADER_NAME: &str = "trg.eval.grader.name";

/// The `[mcp.servers.<name>]` entry an MCP proxy session or OAuth step serves.
pub const MCP_SERVER_NAME: &str = "trg.mcp.server.name";

/// How an MCP proxy session obtained credentials (`none`,
/// `already_authorized`, `authorized`).
pub const MCP_AUTH_OUTCOME: &str = "trg.mcp.auth.outcome";

/// Why an MCP proxy bridge stopped (`host_eof`, `remote_closed`,
/// `local_closed`).
pub const MCP_EXIT_REASON: &str = "trg.mcp.exit.reason";

/// Which way a proxied MCP message travelled (`host_to_remote`,
/// `remote_to_host`).
pub const MCP_MESSAGE_DIRECTION: &str = "trg.mcp.message.direction";

/// How the OAuth loopback callback wait ended (`received`, `timeout`,
/// `state_mismatch`, `provider_error`).
pub const OAUTH_CALLBACK_OUTCOME: &str = "trg.oauth.callback.outcome";

/// The `kind` of the secrets backend an operation addressed (`keychain`,
/// `openbao`, `onepassword`).
pub const SECRETS_BACKEND_KIND: &str = "trg.secrets.backend.kind";

/// The `[secrets.backends.<name>]` entry an operation addressed. A config
/// name, never a path or a value.
pub const SECRETS_BACKEND_NAME: &str = "trg.secrets.backend.name";

/// The fixed subcommand a backend CLI was driven with
/// (`find-generic-password`, `item get`, ...). Never its arguments.
pub const SECRETS_OPERATION: &str = "trg.secrets.operation";

/// How many config vars one secrets fetch served.
pub const SECRETS_VAR_COUNT: &str = "trg.secrets.var.count";

/// How many variables the launched command's environment holds.
pub const EXEC_ENV_VAR_COUNT: &str = "trg.exec.env.var.count";

/// Whether every check a `trg doctor` backend diagnosis ran passed.
pub const DOCTOR_HEALTHY: &str = "trg.doctor.healthy";

/// The content hash of the skill revision an eval suite ran against.
pub const EVAL_SKILL_REVISION: &str = "trg.eval.skill.revision";

/// The scenarios an eval suite covers, comma-separated in suite order.
pub const EVAL_SCENARIOS: &str = "trg.eval.scenarios";

/// How an eval suite is graded once its runs finish (`none`, `auto`). A replayed
/// suite carries the strategy its report was last graded under, so `llm` and
/// `script` appear there too.
pub const EVAL_GRADING_STRATEGY: &str = "trg.eval.grading.strategy";

/// How many runs an eval suite executes at once (`-j`).
pub const EVAL_CONCURRENCY: &str = "trg.eval.concurrency";

/// How many runs an eval suite scheduled.
pub const EVAL_RUN_COUNT: &str = "trg.eval.run.count";

/// The opaque model configuration label a run was recorded under.
pub const EVAL_MODEL_CONFIG: &str = "trg.eval.model_config";

/// The model a run resolved to, once the case's choice and the operator's are combined.
pub const EVAL_RUNNER_MODEL: &str = "trg.eval.runner.model";

/// The harness kind a run executes on (`claude`, `codex`, `cursor-agent`).
pub const EVAL_RUNNER_KIND: &str = "trg.eval.runner.kind";

/// The harness version the availability probe reported.
pub const EVAL_RUNNER_VERSION: &str = "trg.eval.runner.version";

/// A run's final status (`completed`, `failed`, `timeout`, `skipped`).
pub const EVAL_RUN_STATUS: &str = "trg.eval.run.status";

/// What one attempt ended as (`completed`, `transient_failure`, `runner_error`).
pub const EVAL_ATTEMPT_OUTCOME: &str = "trg.eval.attempt.outcome";

/// Whether an attempt failed in a way a retry may recover from.
pub const EVAL_ATTEMPT_TRANSIENT: &str = "trg.eval.attempt.transient";

/// How many files a cache hit restored into the run reusing it.
pub const EVAL_CACHE_RESTORED_FILES: &str = "trg.eval.cache.restored.files";

/// How many bytes a cache hit restored into the run reusing it.
pub const EVAL_CACHE_RESTORED_BYTES: &str = "trg.eval.cache.restored.bytes";

/// Whether the skill directory changed while it was handed to runs.
pub const EVAL_SKILL_TAMPERED: &str = "trg.eval.skill.tampered";

/// The mocked MCP server a `mock-server` process answers as.
pub const EVAL_MOCK_SERVER: &str = "trg.eval.mock.server";

/// How a mocked `tools/call` matched its declaration (`matched`, `violated`,
/// `unresolved`, `unknown_tool`).
pub const EVAL_MOCK_MATCH: &str = "trg.eval.mock.match";

/// What the operator answered when asked to trust a foreign skill (`yes`, `no`,
/// `unanswered`).
pub const EVAL_TRUST_ANSWER: &str = "trg.eval.trust.answer";

/// Counter: eval cache lookups, split by `trg.eval.cache.hit`.
pub const EVAL_CACHE_LOOKUPS_METRIC: &str = "trg.eval.cache.lookups";

/// Counter: runner attempts, split by `trg.eval.attempt.outcome`.
pub const EVAL_ATTEMPTS_METRIC: &str = "trg.eval.attempts";

/// Counter: finished runs, split by `trg.eval.run.status`.
pub const EVAL_RUNS_METRIC: &str = "trg.eval.runs";

/// Counter: USD the harness reported spending, per attempt.
pub const EVAL_COST_METRIC: &str = "trg.eval.cost";

/// The declared type of a grader (`contains`, `llm`, `baseline`, ...), beside
/// `trg.eval.grader.name` on a grader span and on its evaluation event.
pub const EVAL_GRADER_KIND: &str = "trg.eval.grader.kind";

/// The two scenarios a `compare` pair puts side by side, as `{a}:{b}`.
pub const EVAL_COMPARISON_PAIR: &str = "trg.eval.comparison.pair";

/// `true` on every span and evaluation event `eval export` rebuilt from a report
/// bundle after the fact, so a backend can tell a replay from a live pass.
pub const EVAL_REPLAYED: &str = "trg.eval.replayed";

/// The `report.id` of the bundle a replayed `invoke_workflow` span was rebuilt from.
pub const EVAL_REPORT_ID: &str = "trg.eval.report.id";

/// Counter: graded assertions, split by `gen_ai.evaluation.score.label` and
/// `trg.eval.grader.kind`.
pub const EVAL_ASSERTIONS_METRIC: &str = "trg.eval.assertions";
