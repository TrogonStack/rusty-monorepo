# `trg ai skills eval` reference

Command-line interface for running Agent Skills eval suites, writing artifact
bundles, grading them, and verifying workspace outputs.

## Invocation

```text
trg ai skills eval <SUBCOMMAND>
```

| Subcommand | Purpose |
| ---------- | ------- |
| `run` | Validate a skill, scaffold an eval report bundle, optionally invoke an agent runner |
| `grade` | Grade a completed report bundle and write `grading.json` per run |
| `verify` | Validate `grading.json` / `timing.json` files under a workspace tree |
| `init` | Scaffold `evals/evals.json` for a skill directory |
| `benchmark` | Aggregate grading and timing artifacts into `benchmark.json` |
| `iteration-summary` | Summarize assertion stability, skill impact, flakiness, and metric outliers |
| `feedback` | Manage human review feedback artifacts |
| `compare` | Blindly compare scenario outputs within a report directory |
| `next-iteration` | Build an improvement bundle from a prior iteration |

---

## `eval run`

Run skill evals and write an artifact bundle.

```text
trg ai skills eval run --skill-dir <DIR> --out-dir <DIR> [OPTIONS]
```

### Flags

| Flag | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `--skill-dir` | path | *(required)* | Skill directory containing `SKILL.md` and `evals/evals.json` |
| `--out-dir` | path | *(required)* | Root directory for generated artifact bundles |
| `--model-config` | string | `ci-default` | Opaque model-configuration label recorded in `report.json` |
| `--scenario` | enum | `with_skill` | Scenario kind to include. Repeatable; values: `with_skill`, `without_skill`, `old_skill` |
| `--runner` | enum | *(unset)* | Agent CLI to execute each (eval × scenario). When unset, runs are scaffolded with `status: skipped` |
| `--runner-model` | string | *(unset)* | Model identifier forwarded to the runner CLI (`--model` / `-m`). When unset, the runner picks its own default |
| `--force` | bool | `false` | Overwrite an existing report directory if it already exists |
| `--output-format` | enum | `text` | `text` prints a human summary; `json` prints a machine-readable document for the final pipeline stage |

### Runner values

| Value | Program spawned |
| ----- | ----------------- |
| `cursor-agent` | `cursor-agent` |
| `claude-code` | `claude` |
| `codex` | `codex` |

### Exit codes

| Code | Meaning |
| ---- | ------- |
| `0` | Success. Under `text` prints the report directory path on stdout; under `json` prints the document for the final stage that ran |
| `1` | Skill validation, eval-suite validation, bundle write, or runner failure |

### Example (scaffold only)

```shell
$ trg ai skills eval run \
    --skill-dir ./my-skill \
    --out-dir ./artifacts
./artifacts/my-skill/20260526T120000Z-a1b2c3d4
```

### Example (with runner)

```shell
$ trg ai skills eval run \
    --skill-dir ./my-skill \
    --out-dir ./artifacts \
    --runner cursor-agent \
    --runner-model gpt-4.1 \
    --scenario with_skill \
    --scenario without_skill
./artifacts/my-skill/20260526T120530Z-e5f6a7b8
```

### Output layout

```text
<out-dir>/
└── <skill_name>/
    └── <report_id>/
        ├── report.json
        └── runs/
            └── run-001/
                ├── workspace/          # agent working directory
                ├── transcript.jsonl    # redacted runner stdout (when --runner set)
                └── timing.json         # run metrics (when --runner set)
```

Report directories are named `<timestamp>-<random-hex>`. Re-running without
`--force` fails if the same report ID already exists.

---

## `eval verify`

Verify grading and timing artifacts under a workspace directory.

```text
trg ai skills eval verify <WORKSPACE> [OPTIONS]
```

### Positional argument

| Argument | Description |
| -------- | ----------- |
| `WORKSPACE` | Root directory to scan recursively for `grading.json` and `timing.json` |

`eval grade` writes `grading.json` into the run directory, beside `workspace/`
rather than inside it. A path whose last component is `workspace` therefore
widens to its parent before the scan, so pointing `verify` at a workspace finds
the grading for that run.

### Flags

| Flag | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `--mode` | enum | `lenient` | `lenient`: tolerate missing grading files and failed assertions; `strict`: require at least one `grading.json` and fail on failed assertions |
| `--require-assertions` | bool | `false` | Fail when an eval case declares neither an assertion nor a grader |
| `--skill-dir` | path | *(unset)* | Also validate `evals/evals.json` under this skill directory |
| `--output-format` | enum | `text` | `text` prints a human summary; `json` prints a machine-readable document |

`verify` also accepts the threshold and regression flags (`--min-pass-rate`,
`--max-tokens`, `--baseline`, `--strict-ci`, and the `--fail-on-*` family). See
[Pass-rate thresholds](../how-to/run-in-ci.md#pass-rate-thresholds).

### Example (text)

```shell
$ trg ai skills eval verify ./artifacts/my-skill/20260526T120000Z-a1b2c3d4/runs/run-001/workspace
./artifacts/my-skill/20260526T120000Z-a1b2c3d4
assertions: 3/3 passed (100.00%)
ci checks: passed
workspace: ./artifacts/my-skill/20260526T120000Z-a1b2c3d4/runs/run-001/workspace
  grading files: 1
  timing files: 1
```

### Example (JSON)

```shell
$ trg ai skills eval verify ./report/runs/run-001/workspace --output-format json
{
  "report_dir": "./report",
  "exit_code": 0,
  "check": {
    "passed": true,
    "violations": [],
    "metrics": {
      "total_runs": 0,
      "failed_runs": 0,
      "skipped_runs": 0,
      "completed_runs": 0,
      "grading_files": 1,
      "assertion_results": 3,
      "passed_assertions": 3,
      "failed_assertions": 0,
      "pass_rate": 1.0,
      "total_tokens": 0,
      "input_tokens": 0,
      "output_tokens": 0,
      "max_duration_ms": 0,
      "total_duration_ms": 0
    }
  },
  "workspace": {
    "grading_files": 1,
    "timing_files": 1,
    "assertion_results": 3,
    "passed_assertions": 3,
    "failed_assertions": 0,
    "pass_rate": 1.0
  }
}
```

---

## Eval suite manifest (`evals/evals.json`)

Validated before `run` executes. Unknown fields are rejected, and a field is
only accepted from the `schema_version` that introduced it.

| Field | Type | Required | Notes |
| ----- | ---- | -------- | ----- |
| `schema_version` | integer | no | Defaults to `1`. `3` is current and is what `init` scaffolds. `graders` requires `3` |
| `skill_name` | string | yes | Must match the `name` in `SKILL.md` frontmatter |
| `evals` | array | yes | At least one eval case; IDs must be unique |

### Eval case fields

| Field | Type | Required | Notes |
| ----- | ---- | -------- | ----- |
| `id` | string or integer | yes | Non-empty string or non-negative integer |
| `prompt` | string | yes | Non-empty |
| `expected_output` | string | yes | Non-empty reference output for graders |
| `files` | string[] | no | Relative paths inside the skill directory; staged into the run workspace |
| `assertions` | string[] | no | Natural-language checks. Graded mechanically when a known pattern matches, otherwise handed to the LLM judge |
| `graders` | object[] | no | Typed checks (see below). Requires `schema_version` 3 |
| `tags` | string[] | no | Free-form labels |
| `priority` | enum | no | `low`, `medium`, or `high` |
| `timeout_secs` | integer | no | Per-case runner timeout override |
| `expected_output_files` | string[] | no | Files the case is expected to produce |
| `grader_hints` | object | no | Passed through to a script grader on stdin |

A case must declare at least one `assertion` or one `grader`.

---

## Graders

A grader states one checkable property in a form trg can evaluate itself. Unlike
an `assertion`, it is not parsed out of prose, so it means the same thing to
every reader and to every runner.

| `type` | Fields | Checks |
| ------ | ------ | ------ |
| `regex` | `pattern`, `target`, `negate` | The target matches the pattern. Invalid patterns are rejected at manifest parse time |
| `contains` | `text`, `target`, `case`, `negate` | The target contains the text. `case` is `insensitive` (default) or `sensitive` |
| `file_exists` | `path` | The run produced the file |
| `tool_used` | `tool`, `min_calls` | The transcript shows at least `min_calls` calls to the tool |
| `tool_order` | `tools` | The observed tool sequence contains the listed tools in order, as a subsequence |
| `skill_used` | | The run engaged the skill, by a native skill tool call or by reading the staged skill directory |
| `llm` | `criterion` | Handed to the LLM judge, which is the only grader that costs a request |

`target` is `final_text` (default), `transcript`, `any_output`, or
`{"file": "<relative path>"}`.

A relative path resolves against the workspace `outputs/` directory first, then
the workspace itself, then the run directory. Declared outputs therefore win
over an incidental file of the same name, and a plain `summary.md` still
resolves when the agent wrote it straight into its working directory. A path
that matches nowhere reports against the workspace candidate.

### Graders that depend on the transcript

`tool_used`, `tool_order`, and `skill_used` read the normalized transcript.
`claude-code`, `codex`, and `cursor-agent` all expose their tool calls, so all
three answer these graders. When a runner emits no readable stream at all, the
result is recorded as **unsupported**: neither passed nor failed, and excluded
from `pass_rate`, which keeps an unobservable harness from reading as a
regression.

Tool *names* stay in each harness's own vocabulary, because renaming one
harness's tools into another's would assert an equivalence the harness never
made. `codex` reaches the filesystem through a shell, so it reports
`command_execution` and `file_change` rather than `Read` and `Write`. A suite
that must run on every harness should prefer `skill_used`, which is defined in
terms of the staged skill path and answers everywhere. See
[Transcript artifact](#transcript-artifact).

### The LLM judge

`--grader llm`, and an `llm` grader under `--grader auto`, send one request per
assertion to a judge. `compare --judge llm` uses the same machinery.

| Flag | Default | Description |
| ---- | ------- | ----------- |
| `--grader-provider` / `--judge-provider` | `openai` | `openai`, `anthropic`, or `compatible` |
| `--grader-model` / `--judge-model` | *(unset)* | Required whenever a judge is needed |

| Variable | Description |
| -------- | ----------- |
| `TRG_JUDGE_BASE_URL` | Overrides the endpoint. Required for `compatible` |
| `TRG_JUDGE_API_KEY` | Overrides the credential for any provider |
| `OPENAI_API_KEY` | Credential for `openai` when `TRG_JUDGE_API_KEY` is unset |
| `ANTHROPIC_API_KEY` | Credential for `anthropic` when `TRG_JUDGE_API_KEY` is unset |

The judge is chosen independently of `--runner`: grading a `codex` run with an
Anthropic judge, or a `claude-code` run with a local OpenAI-compatible endpoint,
are both ordinary. Under `--grader auto` a suite of typed graders needs no
credential at all, and the endpoint is resolved once up front so a missing
credential is reported before any run is graded.

---

## Artifact: `report.json`

**Status: available.** Always written by `eval run`.

Schema version: `trg.skills-eval.report.v1`. This file is a **superset** of the
agentskills.io report model; companion artifacts (`benchmark.json`,
`grading.json`, etc.) follow the docs shape. See
[Divergences from agentskills.io](../explanation/divergences-from-agentskills-io.md)
for intentional differences and the backward-compatibility contract (fixture
snapshot tests under `crates/trg/src/agentskills/testdata/reports/`).

### Top-level fields

| Field | Type | Description |
| ----- | ---- | ----------- |
| `schema_version` | string | Always `trg.skills-eval.report.v1` |
| `report` | object | Report metadata (id, timestamp, producer, optional CI context) |
| `suite` | object | Skill and eval-suite hashes |
| `dimensions` | object | Eval cases, assertions, scenarios, model configs, skill revisions |
| `runs` | array | One record per (eval case × scenario) |
| `assertion_results` | array | Per-assertion grading outcomes |
| `summaries` | object | Aggregated counts by scenario |
| `comparisons` | array | Cross-scenario comparison records |

`assertion_results` is populated by `eval grade`, which flattens every run's
`grading.json` into it, carrying `unsupported` forward where present.
`comparisons` is populated by `eval compare`. Both are empty until those
subcommands run.

### `report` section

| Field | Type | Description |
| ----- | ---- | ----------- |
| `id` | string | Report identifier (matches directory name) |
| `generated_at` | string | RFC 3339 timestamp |
| `producer.name` | string | Always `trg` |
| `producer.version` | string | `trg` crate version |
| `ci` | object | Present when running inside GitHub Actions (`GITHUB_ACTIONS=true`) |

### `suite` section

| Field | Type | Description |
| ----- | ---- | ----------- |
| `skill_name` | string | From skill frontmatter |
| `skill_path` | string | User-supplied `--skill-dir` path |
| `skill_hash` | string | `sha256:` digest of `SKILL.md` |
| `evals_path` | string | `<skill_path>/evals/evals.json` |
| `evals_hash` | string | `sha256:` digest of `evals.json` |

### `runs[]` record

| Field | Type | Description |
| ----- | ---- | ----------- |
| `id` | string | e.g. `run-001` |
| `eval_case_id` | string | References an eval case id |
| `scenario_id` | enum | `with_skill`, `without_skill`, or `old_skill` |
| `model_config_id` | string | Value of `--model-config` |
| `skill_revision_id` | string | Always `current` today |
| `attempt` | integer | Always `1` today |
| `status` | string | `skipped`, `completed`, or `failed` |
| `paths.workspace` | string | Relative path to the run workspace |
| `artifacts` | array | Artifact descriptors (transcript when runner completes) |
| `metrics` | object | `duration_ms`, token counts, `cost_usd` (populated by runner) |
| `skill_integrity` | object | Tamper detection result (when runner used) |

Run ordering: eval cases in manifest order, then scenarios in flag order.

---

## Artifact: `grading.json`

**Status: available.** Written by `eval grade` (and by `eval run --grade`) as
`runs/<run-id>/grading.json`. `verify` discovers these recursively under a
workspace tree.

Schema version: `trg.skills-eval.grading.v3`. `v2` and `v1` are still accepted
on read. `v2` added `unsupported` and narrowed `pass_rate` to scored results
only; `v3` makes `pass_rate` nullable, because a run where nothing could be
scored has no pass rate and reporting `0.0` reads as a total failure.

```json
{
  "schema_version": "trg.skills-eval.grading.v3",
  "assertion_results": [
    {
      "assertion": "file 'summary.md' exists",
      "passed": true,
      "evidence": "'/abs/path/outputs/summary.md' exists and holds 412 bytes",
      "grader": { "kind": "declarative" }
    },
    {
      "assertion": "the skill was engaged",
      "passed": false,
      "evidence": "runner 'mystery-runner' does not expose tool calls in a form trg can read",
      "grader": { "kind": "declarative" },
      "unsupported": "runner 'mystery-runner' does not expose tool calls in a form trg can read"
    }
  ],
  "summary": {
    "passed": 1,
    "failed": 0,
    "unsupported": 1,
    "total": 2,
    "pass_rate": 1.0
  }
}
```

| Field | Type | Notes |
| ----- | ---- | ----- |
| `assertion_results[].assertion` | string | Non-empty. Accepts `text` as an alias. For a typed grader, its rendered description |
| `assertion_results[].passed` | bool | Pass/fail for this assertion. Always `false` when `unsupported` is present |
| `assertion_results[].evidence` | string | Non-empty. A passing result must not merely restate its assertion |
| `assertion_results[].grader.kind` | enum | `mechanical`, `declarative`, `llm`, `script`, `needs_llm`, or `none` |
| `assertion_results[].rationale` | string | Optional judge reasoning |
| `assertion_results[].unsupported` | string | Present when the runner cannot answer this check. Why it could not be graded |
| `summary.passed` | integer | Must equal the count of scored, passing results |
| `summary.failed` | integer | Must equal the count of scored, failing results |
| `summary.unsupported` | integer | Must equal the count of results carrying `unsupported` |
| `summary.total` | integer | Must equal `assertion_results` length |
| `summary.pass_rate` | float or null | Must equal `passed / (total - unsupported)`, or `null` when nothing was scored |

---

## Artifact: `timing.json`

**Status: available.** Written by agent runners (`cursor-agent`, `claude-code`,
`codex`) alongside each run when `--runner` is set.

Location: `runs/<run-id>/timing.json` (sibling of `workspace/`).

```json
{
  "duration_ms": 1234,
  "total_tokens": 150
}
```

| Field | Type | Required | Notes |
| ----- | ---- | -------- | ----- |
| `duration_ms` | integer | yes | Must be > 0 |
| `total_tokens` | integer | no | When present, must be > 0 |

Token counts and duration are also copied into `report.json` run metrics after
the runner completes.

---

## Artifact: `benchmark.json`

**Status: available.** Written by `eval benchmark` (and by `eval run
--benchmark`), aggregating the grading and timing artifacts of a report bundle
into per-scenario duration, token, and cost summaries.

---

## Artifact: `feedback.json`

**Status: available.** Managed by `eval feedback`, holding a reviewer identity, a
timestamp, and severity-tagged notes against a report bundle.

---

## Artifact: `comparison.json`

**Status: available.** Written by `eval compare --emit-comparison-json` under
the iteration layout directories, and recorded in the `comparisons` array of
`report.json`.

Each record names the judge that produced it, including which provider
answered, because the same model name can be served by more than one endpoint:

```json
{
  "judge": { "kind": "llm", "provider": "anthropic", "model": "<model id>" }
}
```

Outputs are presented to the judge blindly, as A and B, with the mapping back to
scenarios recorded separately in the same record.

---

## Scenario kinds

| Kind | CLI value | Runner behavior |
| ---- | --------- | --------------- |
| With skill | `with_skill` | Stages skill to `.skill/` in workspace; prompt prefixed with skill frontmatter |
| Without skill | `without_skill` | Raw eval prompt; nothing staged |
| Old skill | `old_skill` | Stages the `--old-skill-dir` revision to `.old-skill/` in the workspace; prompt prefixed with that revision's frontmatter |

### The eval suite is withheld from the workspace

The staged directory holds the skill under test minus its top-level `evals/`
directory. The suite is the answer key: it carries each case's
`expected_output`, its natural-language assertions, and its graders' literal
`contains` text and `regex` patterns. A run that could read it could be scored
on text it copied rather than work it did, and the with-skill prompt points the
agent straight at `.skill/`, so the suite is withheld under both
`--skill-staging symlink` and `--skill-staging copy`.

This costs a case nothing. The fixtures a case names in `files` are staged
separately into the workspace root, and they are the only part of `evals/` a
run is meant to see. Only the top level is filtered, so a nested `evals/`
deeper in the skill tree is treated as the skill's own content and staged
normally.

Because the filter has to skip an entry, `--skill-staging symlink` stages
`.skill/` as a real directory holding one symlink per entry rather than as a
single symlink to the skill root. Staging stays as cheap as it was; a run
reading `.skill/SKILL.md` sees no difference.

`--scenario old_skill` requires `--old-skill-dir`. The old skill must carry the
same `name` as the current one unless you pass `--allow-skill-name-mismatch`,
which guards against comparing two unrelated skills by accident. Tampering
detection is scoped to the old skill directory for these runs.

---

## Transcript artifact

When `--runner` is set, runner stdout is written to
`runs/<run-id>/transcript.jsonl` with secrets redacted. A descriptor is appended to the run's
`artifacts` array in `report.json`:

```json
{ "kind": "transcript", "path": "runs/run-001/transcript.jsonl" }
```

Format is runner-specific stream-json (one JSON object per line).

### Normalized transcript (`events.json`)

Alongside the redacted transcript, each run gets
`runs/<run-id>/events.json`: the same turn reduced to one event vocabulary, so a
grader is written once rather than once per harness.

Schema version: `trg.skills-eval.transcript.v1`.

```json
{
  "schema_version": "trg.skills-eval.transcript.v1",
  "runner": "claude",
  "tool_visibility": "observed",
  "events": [
    { "kind": "tool_call", "tool": "Read", "paths": [".skill/SKILL.md"] },
    { "kind": "assistant_text", "text": "..." },
    { "kind": "terminal", "ok": true }
  ],
  "workspace_escapes": [
    { "tool": "read", "path": "/somewhere/outside/notes.md" }
  ]
}
```

| Field | Type | Notes |
| ----- | ---- | ----- |
| `runner` | string | The program that produced the transcript |
| `tool_visibility` | enum | `observed` or `unavailable` |
| `events[].kind` | enum | `assistant_text`, `tool_call`, or `terminal` |
| `workspace_escapes[]` | array | Paths the run named that resolve outside its workspace; omitted when empty |

Each of the three supported runners is normalized from event shapes verified
against that runner's own output:

| Runner | Events read | Tool vocabulary |
| ------ | ----------- | --------------- |
| `claude-code` | `assistant` blocks, `result` | its own tool names, such as `Read` and `Write` |
| `cursor-agent` | `assistant` blocks, `tool_call` (`started`), `result` | the tool-call member name, such as `read`, `glob`, `edit` |
| `codex` | `item.completed`/`agent_message`, `item.started`/`command_execution`, `item.started`/`file_change`, `turn.completed` | `command_execution` and `file_change` |

`tool_visibility` is the honest part. A runner whose stream trg cannot read at
all reports `unavailable`, and a grader that needs tool calls then returns
**unsupported** rather than a fabricated pass or fail.

`workspace_escapes` is the other honest part. trg invokes each harness's own
CLI, which means it cannot confine that CLI's filesystem access: `cursor-agent`
runs with `--force` and `claude-code` has no sandbox flag, so a run can read a
file from anywhere the invoking user can. What is checked is what a tool named as
a path: a file argument, and the operands of a shell command that name a path
outright, so `cat ~/.codex/skills/demo/SKILL.md` is reported while the
interpreter in `/bin/zsh -lc '...'` is not. A search pattern can mention a path
without naming one, so it is left unchecked. A path beginning with `~` names the
host home directory rather than a directory in the workspace, so it is resolved
against `HOME` before the check. Every escape is also recorded as a run warning
in `report.json`. Detection is the remedy available here; prevention is not.

`events.json` is normalized from the redacted transcript, so it carries no
secret that `transcript.jsonl` had stripped.
