# `trg ai skills eval` reference

Command-line interface for running Agent Skills eval suites, writing artifact
bundles, and verifying workspace outputs. This page lists every subcommand,
flag, and artifact shape supported on the `yordis/eval-2` branch.

## Invocation

```text
trg ai skills eval <SUBCOMMAND>
```

| Subcommand | Purpose |
| ---------- | ------- |
| `run` | Validate a skill, scaffold an eval report bundle, optionally invoke an agent runner |
| `verify` | Validate `grading.json` / `timing.json` files under a workspace tree |

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

### Runner values

| Value | Program spawned |
| ----- | ----------------- |
| `cursor-agent` | `cursor-agent` |
| `claude-code` | `claude` |
| `codex` | `codex` |

### Exit codes

| Code | Meaning |
| ---- | ------- |
| `0` | Success. Prints the report directory path on stdout |
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
                ├── transcript.jsonl    # raw runner stdout (when --runner set)
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

### Flags

| Flag | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `--mode` | enum | `lenient` | `lenient` — tolerate missing grading files and failed assertions; `strict` — require at least one `grading.json` and fail on failed assertions |
| `--format` | enum | `text` | Output format: `text` or `json` |

### Example (text)

```shell
$ trg ai skills eval verify ./artifacts/my-skill/20260526T120000Z-a1b2c3d4/runs/run-001/workspace
Bundle verified
  grading files: 1
  timing files: 1
  assertion results: 3/3 passed (100.00%)
```

### Example (JSON)

```shell
$ trg ai skills eval verify ./report/runs/run-001/workspace --format json
{
  "grading_files": 1,
  "timing_files": 1,
  "assertion_results": 3,
  "passed_assertions": 3,
  "failed_assertions": 0,
  "pass_rate": 1.0
}
```

---

## Eval suite manifest (`evals/evals.json`)

Validated before `run` executes. Unknown fields are rejected.

| Field | Type | Required | Notes |
| ----- | ---- | -------- | ----- |
| `skill_name` | string | yes | Must match the `name` in `SKILL.md` frontmatter |
| `evals` | array | yes | At least one eval case; IDs must be unique |

### Eval case fields

| Field | Type | Required | Notes |
| ----- | ---- | -------- | ----- |
| `id` | string or integer | yes | Non-empty string or non-negative integer |
| `prompt` | string | yes | Non-empty |
| `expected_output` | string | yes | Non-empty reference output for graders |
| `files` | string[] | no | Relative paths inside the skill directory; staged into the run workspace |
| `assertions` | string[] | no | Natural-language checks consumed by graders |

---

## Artifact: `report.json`

**Status: available** — always written by `eval run`.

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

> **Status: planned** — `assertion_results` and `comparisons` are scaffolded as
> empty arrays today. Population requires the grading and comparison PRs.

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

> **Status: planned** — not emitted by `eval run` on `yordis/eval-2`. The
> `verify` subcommand validates this shape when you place files manually or
> when a future grader PR writes them.

Expected location: anywhere under a run workspace (discovered recursively).

```json
{
  "assertion_results": [
    {
      "text": "The output includes a summary",
      "passed": true,
      "evidence": "Found summary.md in workspace"
    }
  ],
  "summary": {
    "passed": 1,
    "failed": 0,
    "total": 1,
    "pass_rate": 1.0
  }
}
```

| Field | Type | Notes |
| ----- | ---- | ----- |
| `assertion_results[].text` | string | Non-empty; matches an assertion from `evals.json` |
| `assertion_results[].passed` | bool | Pass/fail for this assertion |
| `assertion_results[].evidence` | string | Non-empty explanation |
| `summary.passed` | integer | Must equal count of `passed: true` results |
| `summary.failed` | integer | Must equal count of `passed: false` results |
| `summary.total` | integer | Must equal `assertion_results` length |
| `summary.pass_rate` | float | Must equal `passed / total` |

---

## Artifact: `timing.json`

**Status: available** — written by agent runners (`cursor-agent`, `claude-code`,
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

> **Status: planned** — not emitted by the CLI on `yordis/eval-2`. Reserved for
> cross-run latency and cost aggregates in a future benchmark PR.

Expected to capture per-scenario p50/p95 duration, token totals, and cost
summaries across eval cases.

---

## Artifact: `feedback.json`

> **Status: planned** — not emitted by the CLI on `yordis/eval-2`. Reserved for
> structured reviewer or LLM-grader feedback in a future grading PR.

Expected to hold qualitative notes, improvement suggestions, and links to failed
assertions.

---

## Artifact: `comparison.json`

> **Status: planned** — not emitted as a standalone file on `yordis/eval-2`.
> The `comparisons` array inside `report.json` is scaffolded empty. A future
> comparison PR will populate cross-scenario deltas (with-skill vs without-skill
> vs old-skill).

Expected shape (illustrative):

```json
{
  "eval_case_id": "analyze-csv",
  "baseline_scenario": "without_skill",
  "candidate_scenario": "with_skill",
  "assertion_delta": { "gained": 2, "lost": 0, "unchanged": 1 },
  "metrics_delta": { "duration_ms": -450, "total_tokens": 120 }
}
```

---

## Scenario kinds

| Kind | CLI value | Runner behavior |
| ---- | --------- | --------------- |
| With skill | `with_skill` | Symlinks skill to `.skill/` in workspace; prompt prefixed with skill frontmatter |
| Without skill | `without_skill` | Raw eval prompt; no skill symlink |
| Old skill | `old_skill` | Scaffolded in report; **runners reject** with `UnsupportedScenario` today |

> **Status: planned** — full `old_skill` runner support (staging a prior skill
> revision) lands in a future PR. You can include `--scenario old_skill` in
> `run` to reserve report slots, but agent invocation will fail until then.

---

## Transcript artifact

When `--runner` is set, raw runner stdout is written to
`runs/<run-id>/transcript.jsonl`. A descriptor is appended to the run's
`artifacts` array in `report.json`:

```json
{ "kind": "transcript", "path": "runs/run-001/transcript.jsonl" }
```

Format is runner-specific stream-json (one JSON object per line).
