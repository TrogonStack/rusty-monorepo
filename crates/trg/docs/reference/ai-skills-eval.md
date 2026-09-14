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
| `html-report` | Render a local-only, self-contained HTML report over a report bundle |

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
| `--eval-dir` | name | `evals` | Directory under the skill holding the eval suite. Wins over a conflicting `eval_dir` declared in the manifest itself, since the manifest cannot be found before this is settled. Recorded in `report.json` so `grade` and `next-iteration` resolve the same suite without needing the flag themselves |
| `--out-dir` | path | *(required)* | Root directory for generated artifact bundles |
| `--model-config` | string | `ci-default` | Opaque model-configuration label recorded in `report.json` |
| `--scenario` | enum | `with_skill` + `without_skill` | Scenario kind to include. Repeatable; values: `with_skill`, `without_skill`, `old_skill`. See [Choosing which scenarios run](#choosing-which-scenarios-run) |
| `--runner` | enum | *(unset)* | Agent CLI to execute each (eval × scenario). When unset, runs are scaffolded with `status: skipped` |
| `--runner-model` | string | *(unset)* | Model identifier forwarded to the runner CLI (`--model` / `-m`). A case's `model` overrides it. When neither is set, the runner picks its own default |
| `--force` | bool | `false` | Overwrite an existing report directory if it already exists |
| `--output-format` | enum | `text` | `text` prints a human summary; `json` prints a machine-readable document for the final pipeline stage |
| `--environment` | enum | `scrubbed` | How much of the host machine each run may see; values: `scrubbed`, `isolated`, `inherited`. See [Run environment](#run-environment) |
| `--permission` | enum | `workspace_write` | How much a run's harness may do without prompting; values: `workspace_write`, `unrestricted`. See [Run permission](#run-permission) |
| `--timeout-secs` | integer | *(unset)* | Per-run timeout. A case's `timeout_secs` overrides it. See [Timeouts](#timeouts) |
| `--attempts` | integer | *(unset, 3 draws)* | Draw each (case × scenario) cell this many times. Overrides any count a case pinned. See [How many times a cell is drawn](#how-many-times-a-cell-is-drawn) |
| `--concurrency`, `-j` | integer | `1` | Execute this many runs at once, `1` to `8`. See [Running more than one run at a time](#running-more-than-one-run-at-a-time) |
| `--max-cost-usd` | USD | *(unset)* | Refuse to start further runs once the pass has spent this many dollars. Accepts a finite amount greater than zero; a ceiling of zero or less could admit nothing and is refused, as is any ceiling over a runner that publishes no price. See [Bounding what a pass may spend](#bounding-what-a-pass-may-spend) |
| `--no-cache` | bool | `false` | Execute every run instead of serving a completed one. See [Reusing a completed run](#reusing-a-completed-run) |
| `--reuse-completed` | bool | `false` | Serve any completed run for the same case and scenario, whatever model config produced it. See [Reusing a completed run](#reusing-a-completed-run) |
| `--case` | glob | *(unset)* | Cover only the cases whose `id` matches. Repeatable. See [Covering part of a suite](#covering-part-of-a-suite) |
| `--tag` | string | *(unset)* | Cover only the cases carrying this `tags` entry. Repeatable. See [Covering part of a suite](#covering-part-of-a-suite) |
| `--allow-scaffold` | bool | `false` | Run the `scaffold` a case declares. See [The state a case is asking about](#the-state-a-case-is-asking-about) |
| `--trust-skill` | bool | `false` | Run a skill directory from outside this working tree without being asked about it. See [Running a skill from outside your working tree](#running-a-skill-from-outside-your-working-tree) |
| `--require-assertions` | bool | `false` | Fail when an eval case declares neither an assertion nor a grader |
| `--lint-evals` | bool | `false` | Print the suite lint's warnings to stderr. Off by default, so a run that does not ask for them prints none, and they change no exit code either way |

### Runner values

| Value | Program spawned |
| ----- | ----------------- |
| `cursor-agent` | `cursor-agent` |
| `claude-code` | `claude` |
| `codex` | `codex` |

### Exit codes

Every `trg ai skills eval` subcommand reports on the same vocabulary. A red job
has to say which kind of red it is, because a skill that scored badly and a
runner that was never installed ask opposite things of whoever reads the alert.

| Code | Meaning | What CI should do |
| ---- | ------- | ----------------- |
| `0` | Success. Under `text` prints the report directory path on stdout; under `json` prints the document for the final stage that ran | Nothing. The pass ran and every gate it was held to passed |
| `1` | A gate failed: skill validation, eval-suite validation, bundle schema conformance, a run left ungraded, a failing assertion or grade, or a threshold such as `--min-pass-rate`. The only code that means the skill is what to go and look at | Read the report and the diff. The thing under test failed a check that was asked of it |
| `2` | `--max-cost-usd` was set and the pass either had runs refused or spent strictly past the ceiling. A pass that lands exactly on the ceiling having refused nothing exits `0`. Only reported when the pass is otherwise clean, so a genuine assertion or grading failure is never masked by a budget stop. See [Bounding what a pass may spend](#bounding-what-a-pass-may-spend) | Raise the ceiling or narrow the pass, then run again. The coverage is short, not the skill |
| `3` | The tool could not do its job: a runner missing from `PATH`, a report or manifest that could not be read or written, a result that could not be serialized, a secrets backend that could not be wired. Nothing was learned about the skill either way | Repair the job. Do not read it as a regression, and do not read a later `0` as a fix for one |
| `128+N` | A signal took the pass down before it reached a verdict. `trg` hands the signal back to its default disposition rather than answering with a code of its own, so a caller sees `130` for `SIGINT`, `143` for `SIGTERM` and `129` for `SIGHUP` | Treat as no result, and re-run if the pass is still wanted |

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

## `eval grade`

Grade a completed eval report bundle, writing `grading.json` per run.

```text
trg ai skills eval grade <REPORT_DIR> [OPTIONS]
```

### Positional argument

| Argument | Description |
| -------- | ----------- |
| `REPORT_DIR` | Path to a report bundle directory containing `report.json` |

### Flags

| Flag | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `--grader` | enum | `auto` | Grading strategy. Values: `auto`, `none`, `llm`, `script` |
| `--grader-provider` | enum | `openai` | Judge backend for LLM grading. Values: `openai`, `anthropic`, `compatible`. `compatible` addresses any OpenAI-compatible endpoint through `TRG_JUDGE_BASE_URL` and `TRG_JUDGE_API_KEY` |
| `--grader-model` | string | *(unset)* | Model identifier for LLM grading. Falls back to `TRG_JUDGE_MODEL` |
| `--grader-command` | string | *(unset)* | External grader script. Reads JSON from stdin: `{assertion, workspace, outputs, transcript}`. Writes `{passed, evidence, rationale?}` to stdout |
| `--grader-votes` | integer | `1` | Opinions to take from the LLM judge on each assertion, decided by majority. Must be odd, so the panel cannot tie. Costs one judge request per vote per assertion |
| `--strict` | bool | `false` | Fail when evidence is missing or assertions require LLM grading |
| `--output-format` | enum | `text` | `text` prints a human summary; `json` prints a machine-readable document |

### Exit codes

| Code | Meaning |
| ---- | ------- |
| `0` | Grading completed with no failed and no ungraded assertions |
| `1` | Grading itself failed, or completed with a failed or ungraded assertion. An assertion left ungraded, meaning no grader could attempt it, fails the command even when `--strict` is not set |

### Example

```shell
$ trg ai skills eval grade ./artifacts/my-skill/20260526T120000Z-a1b2c3d4
./artifacts/my-skill/20260526T120000Z-a1b2c3d4
Graded 1 run(s)
  assertions: 1/1 passed
```

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
| `--mode` | enum | `lenient` | `lenient`: tolerate missing grading files and failed assertions; `strict`: hold every artifact against its schema, require at least one `grading.json`, and fail on failed assertions. Refused outright on a build compiled without the `schema-validation` feature. See [What strict mode needs from the build](#what-strict-mode-needs-from-the-build) |
| `--require-assertions` | bool | `false` | Fail when an eval case declares neither an assertion nor a grader |
| `--skill-dir` | path | *(unset)* | Also validate `evals/evals.json` under this skill directory |
| `--eval-dir` | name | `evals` | Directory under `--skill-dir` the eval suite is resolved from |
| `--output-format` | enum | `text` | `text` prints a human summary; `json` prints a machine-readable document |

`verify` also accepts the threshold and regression flags (`--min-pass-rate`,
`--max-tokens`, `--baseline`, `--strict-ci`, and the `--fail-on-*` family). See
[Pass-rate thresholds](../how-to/run-in-ci.md#pass-rate-thresholds).

### What strict mode needs from the build

Schema validation is the Cargo feature `schema-validation`, on by default, so an
ordinary `cargo build` and every released binary can validate. A build made with
`--no-default-features` compiles the validator out, and every artifact then
passes without being read.

That is the one failure a verification command must not have quietly, so
`--mode strict` refuses to run on such a build rather than exiting clean on a
bundle nothing examined. `--mode lenient` never claimed to check a schema and is
unaffected. If you see the refusal, rebuild with the feature; do not reach for
`--mode lenient` and read its clean exit as conformance.

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

## `eval init`

Scaffold `evals/evals.json` for a skill directory.

```text
trg ai skills eval init --skill-dir <DIR> [OPTIONS]
```

### Flags

| Flag | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `--skill-dir` | path | *(required)* | Path to a skill directory containing `SKILL.md` |
| `--eval-dir` | name | `evals` | Directory under the skill to scaffold the eval suite into |
| `--force` | bool | `false` | Overwrite an existing `evals/evals.json` |
| `--output-format` | enum | `text` | `text` prints a human summary; `json` prints a machine-readable document |

The scaffold it writes declares two eval cases and passes `eval verify --mode
strict` unmodified.

### Example

```shell
$ trg ai skills eval init --skill-dir ./skills/my-skill
Created ./skills/my-skill/evals/evals.json
```

---

## `eval benchmark`

Aggregate a report bundle's grading and timing artifacts into `benchmark.json`,
written both at the report root and under its iteration layout directory.

```text
trg ai skills eval benchmark <REPORT_DIR> [OPTIONS]
```

### Positional argument

| Argument | Description |
| -------- | ----------- |
| `REPORT_DIR` | Path to the report directory containing `report.json` |

### Flags

| Flag | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `--previous` | path | *(auto-detected)* | Previous iteration report directory for cross-iteration drift detection |
| `--failed-runs` | enum | `bucket` | How to treat runner failures when aggregating pass rates. Values: `bucket` (report failed runs as a separate bucket, apart from completed runs), `exclude` (drop failed and timed-out runs from aggregation), `zero` (fold them into the completed bucket, scored as zero) |
| `--allow-eval-suite-drift` | bool | `false` | Suppress the warning when the eval suite hash differs from the previous iteration report |
| `--output-format` | enum | `text` | `text` prints the report directory path; `json` prints the `benchmark.json` document on stdout |

### Example

```shell
$ trg ai skills eval benchmark ./artifacts/my-skill/20260526T120000Z-abc
./artifacts/my-skill/20260526T120000Z-abc
```

---

## `eval iteration-summary`

Summarize assertion stability, skill impact, flakiness, and metric outliers for
a report bundle, writing `iteration-summary.json`.

```text
trg ai skills eval iteration-summary <REPORT_DIR> [OPTIONS]
```

### Positional argument

| Argument | Description |
| -------- | ----------- |
| `REPORT_DIR` | Path to the report directory containing `report.json` |

### Flags

| Flag | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `--previous` | path | *(auto-detected)* | Previous iteration report directory for cross-iteration comparison |
| `--failed-runs` | enum | `bucket` | How to treat runner failures when aggregating pass rates. Values: `bucket`, `exclude`, `zero`. See [`eval benchmark`](#eval-benchmark) |
| `--output-format` | enum | `text` | `text` prints a human-readable table; `json` prints the `iteration-summary.json` document on stdout |

### Example

```shell
$ trg ai skills eval iteration-summary ./artifacts/my-skill/20260526T120000Z-abc
```

---

## `eval feedback`

Manage human review feedback artifacts for a report bundle. A subcommand
group: `init`, `list`, and `validate`.

```text
trg ai skills eval feedback <SUBCOMMAND>
```

### `eval feedback init`

Scaffold an empty `feedback.json` beside `workspace/` for every run in a
report bundle that does not already have one, then sync the feedback summary
into `report.json`.

```text
trg ai skills eval feedback init <REPORT_DIR> [OPTIONS]
```

#### Positional argument

| Argument | Description |
| -------- | ----------- |
| `REPORT_DIR` | Path to a generated eval report directory containing `report.json` |

#### Flags

| Flag | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `--reviewer` | string | *(defaults to `git config user.email`)* | Reviewer identity recorded in `feedback.json`. The command fails if this is unset and git has no `user.email` configured |
| `--output-format` | enum | `text` | `text` prints a human summary; `json` prints a machine-readable document |

#### Example

```shell
$ trg ai skills eval feedback init ./artifacts/my-skill/20260526T120000Z-abc --reviewer reviewer@example.com
Initialized feedback for 1 run(s) (0 already existed)
```

### `eval feedback list`

List runs in a report bundle that still need human review, meaning they have
no `feedback.json` yet.

```text
trg ai skills eval feedback list <REPORT_DIR> [OPTIONS]
```

#### Positional argument

| Argument | Description |
| -------- | ----------- |
| `REPORT_DIR` | Path to a generated eval report directory containing `report.json` |

#### Flags

| Flag | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `--output-format` | enum | `text` | `text` prints the run IDs pending review, or a message that none are pending; `json` prints a machine-readable document |

#### Exit codes

Exits `0` whether or not any run is pending review. A caller distinguishes
the two by the printed list, or by the `pending` array under `--output-format
json`, not by the exit code.

#### Example

```shell
$ trg ai skills eval feedback list ./artifacts/my-skill/20260526T120000Z-abc
Runs needing review:
  run-001
```

### `eval feedback validate`

Schema-validate every `feedback.json` file in a report bundle, then sync the
feedback summary into `report.json` when validation passes.

```text
trg ai skills eval feedback validate <REPORT_DIR> [OPTIONS]
```

#### Positional argument

| Argument | Description |
| -------- | ----------- |
| `REPORT_DIR` | Path to a generated eval report directory containing `report.json` |

#### Flags

| Flag | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `--output-format` | enum | `text` | `text` prints a human summary or the validation errors; `json` prints a machine-readable document. Under `json`, the verdict rides in the document (`validated`, `errors`) rather than on stderr |

#### Exit codes

| Code | Meaning |
| ---- | ------- |
| `0` | Every `feedback.json` in the bundle validated |
| `1` | At least one `feedback.json` failed validation |

#### Example

```shell
$ trg ai skills eval feedback validate ./artifacts/my-skill/20260526T120000Z-abc
Validated 1 feedback file(s)
```

---

## `eval compare`

Blindly compare scenario outputs within a report directory, judging each pair
with the configured judge and recording the result as a `comparisons[]` entry
in `report.json` (and `comparison.json` under iteration layout directories,
with `--emit-comparison-json`).

```text
trg ai skills eval compare <REPORT_DIR> [OPTIONS]
```

### Positional argument

| Argument | Description |
| -------- | ----------- |
| `REPORT_DIR` | Path to a generated eval report directory containing `report.json` |

### Flags

| Flag | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `--previous` | path | *(auto-detected)* | Previous iteration report directory for cross-iteration drift detection |
| `--pair` | string | *(unset)* | Scenario pair to compare, as `<A>:<B>`. Repeatable. Each side is `with_skill`, `without_skill`, or `old_skill`, and the two sides must differ |
| `--judge` | enum | `none` | Judge used to decide each pair. Values: `none`, `llm`, `script`. With `none`, or with no `--pair` given, the command runs no comparisons |
| `--judge-provider` | enum | `openai` | Judge backend for `--judge llm`. Values: `openai`, `anthropic`, `compatible`. `compatible` addresses any OpenAI-compatible endpoint through `TRG_JUDGE_BASE_URL` and `TRG_JUDGE_API_KEY` |
| `--judge-model` | string | *(unset)* | Model identifier for LLM judging. Required when `--judge llm`, unless `TRG_JUDGE_MODEL` names one |
| `--judge-command` | string | *(unset)* | External judge command. Reads JSON from stdin, writes JSON to stdout. Required when `--judge script` |
| `--emit-comparison-json` | bool | `false` | Write `comparison.json` under iteration layout directories when present |
| `--allow-eval-suite-drift` | bool | `false` | Suppress the warning when the eval suite hash differs from the previous iteration report |
| `--output-format` | enum | `text` | `text` prints a one-line summary; `json` prints a machine-readable document |

### Example

```shell
$ trg ai skills eval compare ./report --pair with_skill:without_skill --judge none
Comparison skipped
```

---

## `eval next-iteration`

Build an improvement bundle from a prior iteration's report bundle, for skill
revision.

```text
trg ai skills eval next-iteration [REPORT_DIR] [OPTIONS]
```

### Positional argument

| Argument | Description |
| -------- | ----------- |
| `REPORT_DIR` | Path to the previous iteration report directory containing `report.json`. Optional; `--from` is an alternative to it, and one of the two is required |

### Flags

| Flag | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `--from` | path | *(unset)* | Previous iteration report directory, as an alternative to the positional `REPORT_DIR` |
| `--skill-dir` | path | *(defaults to `skill_path` from `report.json`)* | Skill directory used to detect eval suite drift |
| `--eval-dir` | name | `evals` | Directory under `--skill-dir` the current suite is resolved from, for drift detection |
| `--allow-eval-suite-drift` | bool | `false` | Suppress the warning when the current `evals/evals.json` hash differs from the prior iteration |
| `--output-format` | enum | `text` | `text` prints the bundle paths; `json` prints a machine-readable document |

### Exit codes

| Code | Meaning |
| ---- | ------- |
| `0` | Bundle written |
| `1` | Neither `REPORT_DIR` nor `--from` was given, or building the bundle failed |

### Example

```shell
$ trg ai skills eval next-iteration --from ./artifacts/my-skill/20260526T120000Z-abc
Improvement bundle written to ./artifacts/my-skill/next-iteration
  ./artifacts/my-skill/next-iteration/improvement.md
  ./artifacts/my-skill/next-iteration/improvement.json
```

---

## `eval mock-server`

The subcommand a run's generated `--mcp-config` names as the mock MCP
server's `command`, one invocation per mocked server. See [How a run drives a
mock](#how-a-run-drives-a-mock).

It is hidden from `--help`: `eval run` is the only thing that generates the
`--mocks`, `--server`, and `--calls` arguments it needs, so it exists for a
harness to invoke on the operator's behalf rather than for an operator to
type by hand.

---

## `eval html-report`

Render a local-only, self-contained HTML report over a report bundle.

```text
trg ai skills eval html-report <REPORT_DIR> [OPTIONS]
```

### Positional argument

| Argument | Description |
| -------- | ----------- |
| `REPORT_DIR` | Path to the report directory containing `report.json` |

### Flags

| Flag | Type | Default | Description |
| ---- | ---- | ------- | ----------- |
| `--output-format` | enum | `text` | `text` prints the report directory path and the HTML file path; `json` prints a machine-readable document |

### Example

```shell
$ trg ai skills eval html-report ./artifacts/my-skill/20260526T120000Z-abc
./artifacts/my-skill/20260526T120000Z-abc
html report: ./artifacts/my-skill/20260526T120000Z-abc/report.html
```

---

## Eval suite manifest (`evals/evals.json`)

Validated before `run` executes. Unknown fields are rejected.

| Field | Type | Required | Notes |
| ----- | ---- | -------- | ----- |
| `schema_version` | integer | no | Accepted for backward compatibility; has no effect on parsing |
| `skill_name` | string | yes | Must match the `name` in `SKILL.md` frontmatter |
| `eval_dir` | string | no | A single path segment, no `/`, `\`, `.`, or `..`. Not a way to relocate the suite: the directory it is found in (`--eval-dir` or its default) already had to be settled to find this manifest at all. Declaring it here is checked only for agreement with that value, and a mismatch is rejected rather than silently overridden, so a manifest cannot claim to live somewhere other than where it was found |
| `evals` | array | yes | At least one eval case; IDs must be unique |

### Eval case fields

| Field | Type | Required | Notes |
| ----- | ---- | -------- | ----- |
| `id` | string or integer | yes | Non-empty string or non-negative integer |
| `name` | string | no | Non-empty. For humans reading a report; `id` remains the key |
| `description` | string | no | Non-empty. For humans reading a report |
| `prompt` | string | yes | Non-empty |
| `expected_output` | string | yes | Non-empty reference output for graders |
| `files` | (string \| object)[] | no | Relative paths inside the skill directory, staged into the run workspace. A bare string is writable; an object names `path` and `mode` (`writable`, the default, or `read_only`). See [Read-only fixtures](#read-only-fixtures) |
| `assertions` | string[] | no | Natural-language checks, retained for compatibility. `graders` is the supported mechanism. Under `--grader auto` (the default) and `--grader none`, graded mechanically when a known pattern matches and recorded ungraded when none does; `--grader llm` sends it to the judge and `--grader script` to the script grader. See [Prose assertions](#prose-assertions) |
| `graders` | object[] | no | Typed checks (see below) |
| `skill_disclosure` | enum | no | `announced` (default) or `unannounced`. See [Measuring triggering](#measuring-triggering) |
| `companion_skills` | string[] | no | Relative paths to other skill directories inside the skill directory, staged beside the one under test so an unannounced case has something to pass over. Only an unannounced case may declare them. See [Skills staged only to be passed over](#skills-staged-only-to-be-passed-over) |
| `tags` | string[] | no | Free-form labels. `--tag` selects by them. See [Covering part of a suite](#covering-part-of-a-suite) |
| `priority` | enum | no | `low`, `normal`, `high`, or `critical` |
| `timeout_secs` | integer | no | Per-case runner timeout override |
| `attempts` | integer | no | How many times this case is drawn, for a case whose stability is the question or whose cost makes the suite default too expensive. At least 1. Yields to an explicit `--attempts`. See [How many times a cell is drawn](#how-many-times-a-cell-is-drawn) |
| `model` | string | no | The model this case wants, for a case whose question is about one model in particular. Overrides `--runner-model` |
| `env` | object | no | Variables added to this case's own run. Names are confined to `EVAL_*`. See [Variables a case sets](#variables-a-case-sets) |
| `append_system_prompt` | string | no | Text appended to the harness's own system prompt for this case. Not blank. Only `claude-code` can carry it; elsewhere the case is skipped. See [Steering a case's system prompt](#steering-a-cases-system-prompt) |
| `expected_output_files` | string[] | no | Files the case is expected to produce |
| `grader_hints` | object | no | Passed through to a script grader on stdin |
| `scaffold` | string | no | Relative path to a script inside the skill directory, run in the workspace before the agent starts. Requires `--allow-scaffold`. See [The state a case is asking about](#the-state-a-case-is-asking-about) |
| `conversation_history` | string | no | Relative path to a transcript inside the skill directory, to resume before the case's prompt. No installed harness can adopt an arbitrary transcript as its own history, so a case that sets this is skipped rather than run. See [Seeding a conversation](#seeding-a-conversation) |

A case must declare at least one `assertion` or one `grader`.

### Prose assertions

`graders` is the supported way to state what a case checks. `assertions` is the
older surface and is kept only so that suites written before typed graders
existed keep running unchanged; it is still parsed, still graded, and still
scored exactly as it always was, because removing a field that published suites
already carry would be a breaking change and `trg` does not make one before v1.

The reason to move is what the two mechanisms do with a check they cannot
parse. A typed grader is either understood or rejected outright, at load time,
by name. A prose assertion is matched against a fixed set of phrasings, and one
that matches none of them is not an error: under the default `--grader auto` it
is recorded `ungraded`, which is neither a pass nor a failure. It measures
nothing about the skill, it is left out of `pass_rate`, and it still turns the
pass red, so a mistyped assertion reads as a broken harness rather than as a
finding about the skill it was meant to check. Only `--grader llm` sends an
unrecognized assertion to the judge.

A case declaring `assertions` is warned by the suite lint, which names `graders`
as the replacement. `eval verify` lints every time; `eval run` lints only when
`--lint-evals` is passed, so a run that does not ask for the lint prints no
warning. The warning changes no exit code either way.

An existing suite does not have to be rewritten to keep working. The reference
for what the prose forms are and how each is matched is unchanged; see
[Graders](#graders) for the typed equivalents.

### Eval case directories (`evals/<case-id>/`)

An alternative to `evals/evals.json`: one directory per case instead of one JSON
array. A skill's `evals/` directory may use either layout, but not both; mixing
a manifest with case directories in the same suite is rejected, naming both
conflicting sources.

```
<skill>/evals/
  <case-id>/
    prompt.md             # required, becomes the case's `prompt`
    case.json             # optional, the case's non-prose fields
    graders/
      <name>.json          # optional, one CaseGrader per file
```

| File | Required | Notes |
| ---- | -------- | ----- |
| `prompt.md` | yes | Its contents become `prompt`. A directory without it is rejected by name |
| `case.json` | no | Any `EvalCase` field other than `id`, `prompt`, and `graders`. Redeclaring one of those three is rejected |
| `graders/*.json` | no | One `CaseGrader` per file. A file's stem becomes the grader's `name` when the file does not declare one itself |

The directory name becomes the case `id`. A directory that holds none of
`prompt.md`, `case.json`, or `graders/` is not treated as a case, which keeps a
directory of unrelated fixtures (such as `evals/files/`) out of the suite.

Both layouts compile to the same `EvalSuite` and `EvalCase` shapes, so nothing
downstream (grading, reports, drift detection) can tell which one a suite used.
What differs is `evals_hash`: a manifest keeps hashing the raw file bytes of
`evals.json`, while case directories hash a canonical walk of every case's
files in sorted order, under a distinct regime tag. The two can never produce
the same hash for the same logical content, so moving a suite from one layout
to the other always reads as a change, which is honest: its source did change.

---

## MCP mocks

A case can declare what a call to an MCP tool should answer with, instead of
reaching a real MCP server. Only fixed mocks are supported: a mock always
answers the same way, keyed only on the tool being called, not on what the
call was asked to do. Record/replay and agent-driven mocks are a different
feature and are not implemented; a suite that tries to declare one gets a
named error rather than a silent fallback.

```
<skill>/evals/
  mocks/
    <server>/
      <tool>.md            # suite-level: every case gets this unless it overrides it
  <case-id>/
    mocks/
      <server>/
        <tool>.md           # per-case: replaces the suite-level file for this one tool
```

A per-case override replaces the suite-level file for that exact
`<server>/<tool>` pair; it does not merge with it. A tool with no mock at
either level is not intercepted at all, so a case can mock some of a skill's
tools and let the rest reach whatever the harness would otherwise reach.

A `mocks/`-only directory is never mistaken for a case: `evals/mocks/` has
none of `prompt.md`, `case.json`, or `graders/`, the only markers that make a
directory under `evals/` a case, so it coexists safely with either suite
layout above.

### Mock declaration format

Each `<tool>.md` is frontmatter plus a body, the same shape as a grader file:

```markdown
---
type: fixed
expect:
  repo: acme/widgets
  title: /^feat:/
  labels: [bug, enhancement]
---
{"issue_number": 42, "url": "https://github.com/acme/widgets/issues/42"}
```

| Field | Type | Required | Notes |
| ----- | ---- | -------- | ----- |
| `type` | string | yes | Only `fixed` is implemented |
| `expect` | object | no | Dotted path into the call's input, mapped to a constraint |
| `error` | string | no | When present, the call answers as an MCP tool error with this message instead of the body |

The body is the tool's result: literal JSON if the tool result should be JSON,
or plain text otherwise. It supports two substitutions:

- `{{input.<dotted.path>}}`, resolved against the actual call's arguments when
  the call happens. A path with no matching value is a call-time error, not an
  empty string: a mock that silently answers with nothing a skill never asked
  for is a harder bug to notice than one that fails loudly.
- `{{file:<relative path>}}`, resolved once when the mock declaration is
  loaded, relative to the directory the `.md` file is in. A missing file is
  also an error rather than an empty body.

An `expect` constraint is written as a bare string and its shape decides what
it means: `/pattern/` (opening and closing slashes) is a regex, one of
`string`, `number`, `boolean`, `object`, `array` is a type check against the
value's JSON type, and anything else is matched literally. A YAML list of
strings is a one-of: the value must equal one of them. A path the input never
carries is a violation like any other, with the received value reported as
`null`. A `/pattern/` is compiled when the declaration is loaded, so a regex
that does not compile fails the suite by name rather than becoming an
expectation no call can ever satisfy.

A constraint violation never stops the mock from answering the call; the
mock still returns `body` (or `error`), because otherwise it could not tell
the skill's next turn what it thinks its previous turn asked for. Instead,
every call the mock server answers is logged to `mock-calls.jsonl` under the
run's own directory, one line per call, and every violation on that log
becomes an ordinary failing assertion during grading: no MCP mock aspect on
its own can fail a run outside grading, since introducing a second failure
channel that competed with ordinary assertions would only give the same
outcome two different ways to be reported.

### How a run drives a mock

trg materializes the resolved mock set for a run into that run's own
directory and generates an `--mcp-config` document that points each declared
server at trg's own hidden `mock-server` subcommand, one invocation per
server, so a harness never has to know a mock exists as anything other than
an MCP server on stdio. Each mock server creates `mock-calls.jsonl` as soon as
it has loaded its mocks and before any call can reach it, so a reader can tell
"declared but unused" from "no mock server ever came up": the first leaves an
empty log, the second leaves no log at all. A grader aimed at the
[`mock_calls` target](#grading-what-the-agent-asked-for) reads that same
distinction.

Resolved mock content is folded into the run's cache key. Changing a mock's
`expect` map, body, or error message invalidates a cache entry the same way
changing the prompt does, since the mock is as much an input to the run as
the prompt is.

A harness whose `mcp servers` cell in the [Harness support](#harness-support)
table is `no` cannot be driven with mocks at all. A case that declares mocks
against such a harness is not attempted and is not reported as a failure: the
run is skipped with `mcp_unsupported`, and grading skips it the same way it
skips a run stopped by the cost ceiling, rather than reading the run's
absence as a wrong answer.

### Mocks on `codex`

`codex` needs `--environment isolated` before it can be driven with mocks. It
takes an MCP server table only as the `config.toml` of the config home named
by `CODEX_HOME`, and it offers no flag that excludes the MCP servers already
configured there, so the operator's own servers would stay reachable beside the
mocks and the run would not be the run its report describes. `--environment
isolated` is the policy that gives a run a config home of its own, which is the
only place trg will declare mock servers for `codex`.

A mocked `codex` case under `--environment scrubbed` or `--environment
inherited` is refused the same way an unsupported control is: the run is
skipped with the `unsupported` failure kind, and its warning names the policy
that would let it run. trg never writes into the config home the operator owns.

---

## Graders

A grader states one checkable property in a form trg can evaluate itself. Unlike
an `assertion`, it is not parsed out of prose, so it means the same thing to
every reader and to every runner.

| `type` | Fields | Checks |
| ------ | ------ | ------ |
| `regex` | `pattern`, `target`, `negate`, `flags`, `count` | The target matches the pattern. Invalid patterns are rejected at manifest parse time |
| `contains` | `text`, `target`, `case`, `negate` | The target contains the text. `case` is `insensitive` (default) or `sensitive` |
| `file_exists` | `path`, `exists` | The run produced a file at `path`, which may be a glob. `exists` defaults to `true`; set it to `false` to assert that nothing matches |
| `tool_used` | `tool`, `input_match`, `min_calls`, `max_calls` | The transcript shows between `min_calls` (default 1) and `max_calls` (default unbounded) calls to the tool, inclusive. With `input_match`, only the calls that named a value matching that pattern are counted |
| `tool_order` | `tools`, or `before`/`after` | The observed tool sequence contains the listed tools in order, as a subsequence; or, with `before`/`after`, some call to `before` precedes some later call to `after` |
| `skill_used` | `negate` | The run engaged the skill, by a native skill tool call naming the staged skill or by reading the staged skill directory |
| `llm` | `criterion`, `target` | Handed to the LLM judge, which is the only grader that costs a request |
| `valid_json` | `target` | The target parses as JSON. Evidence carries the line and column of the first parse error |
| `schema_validation` | `schema`, `target` | The target parses as JSON and validates against the named JSON Schema document. `schema` is a relative path inside the skill directory, resolved the same way `files` and `scaffold` are |
| `baseline` | `reference`, `criterion`, `target` | The target is at least as good as a reference output already in the suite, judged on `criterion`. Costs judge requests. See [Holding a run to a reference output](#holding-a-run-to-a-reference-output) |

Every grader also accepts `arm`, which decides whether its result counts toward
the score. See [Arm-scoped graders](#arm-scoped-graders).

Every grader also accepts `name`, a non-empty label unique within its case. A
grader's only other identity in results is its rendered description, so editing
a pattern or a threshold would otherwise silently change the key a downstream
comparison keys on; `name` gives it a stable one. Two graders in the same case
declaring the same `name` are rejected. In the directory layout, a grader
file's stem is its default name.

Every grader also accepts `weight`, a number greater than zero. A case's score
is the fraction of weight it passed rather than a plain count, so a grader
worth three times as much as the rest of the case declares `"weight": 3`. A
grader that leaves `weight` undeclared counts as one full vote, exactly what
every grader counted as before weighting existed, so a case that never opts in
scores exactly as it always has. Zero and negative weight are both rejected at
parse time: a grader worth nothing to the score belongs out of the case
entirely (`"arm": "with_only"`) rather than weighted to zero, and a negative
weight has no share of a score to subtract from.

`target` is `final_text` (default), `transcript`, `any_output`,
`{"file": "<relative path>"}`, `{"files": "<glob>"}`, `created_files`, or
`mock_calls`.
`created_files` is the set of paths the run wrote under `outputs/`, read from
the same index the report itself is built from, so checking that the agent
created a file named `X` never re-walks the output directory.

`{"files": "<glob>"}` is `any_output` narrowed to the files a pattern names,
using the same wildcard dialect `file_exists` accepts. The matching files are
read in path order and each is labelled with the path it came from, so the same
run grades the same way twice and a verdict about one file is attributable to
it. A file that cannot be read as UTF-8 text is labelled and named by size
rather than skipped, since a label with nothing under it would read as a file
the agent left empty.

A leading `outputs/` is dropped before matching, the same way `file_exists`
drops it, so `{"files": "outputs/**/*.md"}` and `{"files": "**/*.md"}` name the
same set.

Unlike `file_exists`, a `files` target searches only `outputs/`, never the
workspace around it. Narrowing `any_output` must not read more than
`any_output` does, and the workspace holds the agent's scratch files and the
copy of the skill the harness staged. A pattern that matches nothing is a
target that is **missing**, not one that is empty, so it fails where it is read
instead of reaching a judge with nothing to look at.

A relative path resolves against the workspace `outputs/` directory first, then
the workspace itself, then the run directory. Declared outputs therefore win
over an incidental file of the same name, and a plain `summary.md` still
resolves when the agent wrote it straight into its working directory. A path
that matches nowhere reports against the workspace candidate.

An `llm` grader whose criterion text also happens to parse as a known
mechanical pattern (see the `assertions` row above) is graded mechanically
under `--grader auto` and `--grader llm` alike, as a shortcut that skips the
judge request. Declaring `target` on that grader turns the shortcut off, even
when the declared value is `final_text`, the same value the field would have
defaulted to: writing `target` at all is the author saying what to look at,
which the shortcut cannot promise to honor since it grades from the criterion
text rather than the declared target.

### Grading what the agent asked for

`mock_calls` is the one target that grades the request rather than the answer.
Every other target describes what the agent produced; this one describes what
it asked an [MCP mock](#mcp-mocks) for, so a case can hold a skill to the
arguments it sends and not only to the prose it writes afterwards.

The target is every call the run made, in the order the mock server answered
them, one call per line:

```
<server>.<tool> <input as compact json>
```

For example:

```
github.create_issue {"labels":["bug"],"repo":"acme/widgets","title":"Flaky build"}
github.close_issue {"number":41}
```

The rendering is the surface patterns are written against, not the raw
`mock-calls.jsonl` the mock server writes. A pattern aimed at that file would
be answering questions about field order and whitespace as much as about the
call, and it would break the first time the log grew a field. Object keys are
rendered in sorted order, so the same call renders the same way twice:

```json
[
  { "type": "regex", "pattern": "^github\\.create_issue .*\"repo\":\"acme/", "target": "mock_calls" },
  { "type": "contains", "text": "github.close_issue", "target": "mock_calls", "negate": true }
]
```

`expect` violations are deliberately absent from the rendering. A run already
reports each one as its own failing assertion, and repeating them here would
let a single mismatch fail a case twice.

A run that hosted its mocks and called none of them is an **empty** target, not
a missing one. That is a real fact about the agent, and stating it as emptiness
is what lets `negate` keep meaning what it says: "the agent never asked for a
force push" has to pass on a run that asked for nothing at all.

A run that left no call log at all hosted no mock server, and so has no such
fact to report either way. The skill is never credited or blamed for a mock
that was not there, but how that reads in a report depends on whether the run
reached the harness, and the two are not interchangeable:

- A case that declares mocks against a harness whose `mcp servers` cell is `no`
  is never attempted. The run is reported with status `skipped` and
  `mcp_unsupported`, and grading skips it whole, so **no assertion is created
  for it at all**: it is counted in neither `passed`, `failed`, nor
  `unsupported`.
- A run that did reach the harness and still left no log, because the case
  declares no mocks or because no mock server came up, is graded like any other.
  **The assertion is created and comes back `unsupported`**, counted in
  `unsupported` and left out of the pass rate the way every unsupported
  assertion is.

A log that is present but unreadable is neither of those. The run did host a
mock server, so the assertion **fails** with the read error as its evidence,
the same as any other target trg could not read.

### Asserting which command ran, not just that a tool was used

A tool name on its own says "a shell ran", which is rarely what a case means.
`input_match` is a regular expression, and only the calls that named a value
matching it are counted:

```json
{ "type": "tool_used", "tool": "Bash", "input_match": "npm (run )?test" }
```

The pattern is tested against the values the call named, one at a time: the
paths it was given, the command lines it ran, and the texts it searched for.
Those are what the normalized transcript keeps. It is deliberately not tested
against the JSON body a harness sent, because the key a command arrives under
differs per harness, so a pattern written against one harness's request body
would quietly match nothing under another. See
[Transcript artifact](#transcript-artifact).

An unusable pattern is rejected when the manifest is parsed, not when the
grader runs.

### Matching a file by name, or asserting one is absent

`file_exists`'s `path` accepts the same wildcards a shell glob does, searched
under `outputs/` first and then the workspace:

| Wildcard | Matches |
| -------- | ------- |
| `*` | Any run of characters other than `/`, within one path segment |
| `**/` | Zero or more whole path segments |
| `**` | Any run of characters, including `/`, when it is not followed by `/` |
| `?` | Any single character other than `/` |

```json
{ "type": "file_exists", "path": "outputs/**/*.md" }
```

A literal `path` is resolved against the usual `outputs/`, workspace,
run-directory order, but a glob searches neither the run directory nor the
directory the skill was staged in. A literal path names one file, and an author
who writes `transcript.jsonl` means it; a glob is a description of what the run
should have produced, and `timing.json` at the run root or the staged `SKILL.md`
would answer it with a file the agent never wrote. A leading `./` is dropped
before matching, so `./*.md` and `*.md` are the same pattern.

A leading `outputs/` is dropped before the output tree is searched, and kept for
the workspace. `outputs/` names that tree rather than a directory that is always
there to walk into: a run may leave it beside the workspace or inside it, and in
the first layout no workspace-relative path begins with `outputs/` at all. A
pattern naming it therefore answers the same way under either layout, which is
how a literal `outputs/...` path already behaves. The prefix is dropped only for
the output tree, so `outputs/*.md` is still not answered by a stray `notes.md`
the agent left in its working directory.

Character classes (`[...]`), brace expansion (`{...}`), and escaping (`\`) are
not part of this dialect; a pattern containing `[`, `]`, `{`, `}`, or `\` is
rejected when the manifest is parsed; use `*` and `?` for wildcards. A `path`
with none of `*` or `?` is a literal path, matched the same way it always was.

`exists` defaults to `true`. Setting it to `false` turns the same grader into
its own negation, for asserting that a run did *not* produce a matching file,
without reaching for a `negate` field that `file_exists` has never had:

```json
{ "type": "file_exists", "path": "outputs/*.tmp", "exists": false }
```

### Regex flags and exact match counts

`regex` accepts `flags`, a string combining any of `i` (case-insensitive), `m`
(`^`/`$` match at line boundaries, not only at the start and end of the whole
target), and `s` (`.` also matches newlines). These are the modes the `regex`
crate itself exposes; any other letter is rejected when the manifest is
parsed.

```json
{ "type": "regex", "pattern": "^error:", "target": "transcript", "flags": "im" }
```

`count` asks for an exact number of non-overlapping matches instead of "at
least one":

```json
{ "type": "regex", "pattern": "TODO", "count": 3 }
```

Combined with `negate`, `count` asks for anything other than that number.
Without `count`, `regex` keeps checking for presence, as it always has.
`count` must be at least 1: "the pattern never appears" is already what
`negate` without a `count` means, so `count: 0` would only be a second way to
write the same check and is rejected when the manifest is parsed.

### Stating that a tool or the skill must not be reached for

A skill that answers a prompt it was never meant to answer is as much a defect
as one that never fires, and a lower bound alone cannot say so, since every
number of calls satisfies it. `max_calls` is the upper half:

```json
{ "type": "tool_used", "tool": "WebSearch", "min_calls": 0, "max_calls": 0 }
```

`min_calls: 0` on its own is refused. It accepts every run, and a check that
cannot fail reads in a report exactly like one that held. So is a `max_calls`
below `min_calls`, which no run can satisfy.

Pairing the two bounds with `input_match` narrows the refusal to one command
rather than to the whole tool, which is usually what a case means: a skill may
legitimately reach for a shell and still must not reach for this.

```json
{ "type": "tool_used", "tool": "Bash", "input_match": "^git push", "min_calls": 0, "max_calls": 0 }
```

Tool names belong to one harness's vocabulary, so the portable form of "the
skill must not be reached for" is `skill_used` with `negate`, which answers on
every runner. See [Arm-scoped graders](#arm-scoped-graders).

### Ordering two specific calls, not a whole sequence

`tools` asks for a subsequence, which is the right shape when a case cares
about several steps happening in order. When it only cares that one call
happened before another, `before`/`after` says that directly instead of
padding `tools` out to two entries:

```json
{ "type": "tool_order", "before": "Read", "after": "Write" }
```

Either side accepts the qualified form `tool_used` does, to ask about a
specific call rather than any call to that tool:

```json
{
  "type": "tool_order",
  "before": { "tool": "Read", "input_match": "config\\.json$" },
  "after": { "tool": "Bash", "input_match": "npm test" }
}
```

The check holds when some call matching `before` is followed, later in the
transcript, by some call matching `after`; neither needs to be the only call
to its tool. `tools` and `before`/`after` are mutually exclusive, and
`before`/`after` must both be given together.

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

### Arm-scoped graders

A `skill_used` grader states a property that cannot hold unless the skill is
staged. Scored as written, it passes in every with-skill run and fails in every
without-skill run for no reason but its own premise, and the gap `benchmark.json`
reports between the two arms widens by exactly the number of such graders. The
skill gets credit for being present rather than for the work it changed.

So a grader that presupposes the skill is evaluated and reported in both arms
and scored in neither. Its result carries `excluded` in `grading.json`, counts
in `summary.excluded`, and stays out of `summary.pass_rate`. Read it as an
indicator: whether the run reached for the skill. In the without-skill arm the
answer is no by construction, because that arm stages no skill for a run to
reach for.

| `arm` | Meaning |
| ----- | ------- |
| `auto` (default) | `skill_used` is reported but not scored; every other grader is scored |
| `with_only` | Reported in both arms, scored in neither, whatever the grader checks |
| `both` | Scored in both arms even though the grader presupposes the skill |

`arm: both` is how a negative expectation is written: a case whose point is that
the skill must *not* be engaged needs its `skill_used` grader scored, because
failing it is the finding. Pair it with `negate` so the check reads the way the
case means it:

```json
{ "type": "skill_used", "negate": true, "arm": "both" }
```

A negated engagement check is settled by the arm just as much as a plain one,
which is why it still needs `arm: both`: there is no skill to engage in the
without-skill arm, so "not engaged" holds there for a run that did nothing at
all.

`arm: with_only` is the manual form for anything trg cannot recognise on its
own. Tool names belong to one harness's vocabulary, so a `tool_used` grader
naming a harness's skill-invocation tool has to be marked by hand; `skill_used`
is the portable way to state the same thing and needs no marking.

If every check in a case would be excluded, the case would measure nothing at
all, which is never what writing it meant. The exclusions are lifted and the
case is scored as declared, so a suite whose only check is `skill_used` still
produces a score. Such a case does widen the arm gap, and that is the author's
declared intent rather than an accident of scaffolding: a triggering case is
asking exactly whether the skill was reached for. Write it with
`"skill_disclosure": "unannounced"` so the answer is the run's own, not the
prompt's, and name `companion_skills` so the run had something else it could
have reached for. See [Measuring triggering](#measuring-triggering).

### The LLM judge

`--grader llm`, and an `llm` grader under `--grader auto`, send one request per
assertion to a judge. `compare --judge llm` uses the same machinery.

| Flag | Default | Description |
| ---- | ------- | ----------- |
| `--grader-provider` / `--judge-provider` | `openai` | `openai`, `anthropic`, or `compatible` |
| `--grader-model` / `--judge-model` | *(unset)* | Required whenever a judge is needed, unless `TRG_JUDGE_MODEL` names one |
| `--grader-votes` | `1` | Opinions taken per assertion, decided by majority. Odd values only. See [Asking the judge more than once](#asking-the-judge-more-than-once) |

| Variable | Description |
| -------- | ----------- |
| `TRG_JUDGE_BASE_URL` | Overrides the endpoint. Required for `compatible` |
| `TRG_JUDGE_MODEL` | Model to judge with when no `--grader-model` or `--judge-model` is passed |
| `TRG_JUDGE_API_KEY` | Overrides the credential for any provider |
| `OPENAI_API_KEY` | Credential for `openai` when `TRG_JUDGE_API_KEY` is unset |
| `ANTHROPIC_API_KEY` | Credential for `anthropic` when `TRG_JUDGE_API_KEY` is unset |

A judge is addressed by an endpoint, a credential, and a model. The first two
already come from the environment, so `TRG_JUDGE_MODEL` is what lets a judged
pass run without naming a model on every invocation. A flag still wins where one
is passed, and the model recorded in `grading.json` and `comparison.json` is
whichever one actually answered, not the flag. There is no built-in default: a
model identifier compiled into a release outlives the model it names.

The judge is chosen independently of `--runner`: grading a `codex` run with an
Anthropic judge, or a `claude-code` run with a local OpenAI-compatible endpoint,
are both ordinary. Under `--grader auto` a suite of typed graders needs no
credential at all, and the endpoint is resolved once up front so a missing
credential is reported before any run is graded.

#### What the judge is shown

A prose assertion, and an `llm` grader that does not declare `target`, get the
default `final_text` payload, unchanged from before `target` existed:

```json
{ "assertion": "...", "final_text": "...", "outputs": { "<name>": "..." } }
```

`any_output` gets the same shape, and now the same scope: `outputs` walks
every file under `outputs/`, nested directories included, matching what a
mechanical grader aimed at `any_output` already looks at. `final_text` (declared
or defaulted) keeps looking only at what sits directly in `outputs/`, so a
suite written before `any_output` walked subdirectories still grades the same
way. `transcript`, `{"file": ...}`, `{"files": ...}`, `created_files`, and
`mock_calls` instead get a payload that names its own target, since the judge
is no longer implicitly looking at the run's output:

```json
{ "assertion": "...", "target": "transcript", "content": "..." }
```

A target that resolves to nothing (a named file the run never wrote) reports
why instead of sending an empty string:

```json
{ "assertion": "...", "target": "file 'out.md'", "missing_reason": "..." }
```

A `{"file": ...}` target that resolves to a `.png`, `.jpg`, `.jpeg`, `.gif`, or
`.webp` file is attached to the judge as a picture instead of being read as
text, since reading it as text would only ever fail:

```json
{ "assertion": "...", "target": "file 'chart.png'", "content": "image attached separately" }
```

`regex`, `contains`, `valid_json`, and `schema_validation` aimed at an image
target cannot fall back to a judge request, so each reports why it failed
rather than misreading the bytes as text:

```json
{ "passed": false, "evidence": "file 'chart.png' is an image; a pattern can only match text" }
```

One judge request carries at most 8,000 bytes of content in total, shared
across every artifact placed into it rather than granted per artifact. Each
artifact still to be placed claims an equal share of what remains, so an
artifact that needs less than its share leaves the difference for the ones
after it. An artifact too large for its share is truncated with a trailing
`[truncated after N bytes]` marker; an artifact that arrives after the budget
is already spent is left out of the payload entirely, and its omission is
reported via an `artifacts_omitted` field rather than passing silently. A
transcript keeps its tail and drops its head, since the run's outcome and its
most recent tool calls sit at the end; every other target keeps its head and
drops its tail.

A transcript's cut falls between messages, not through one: it is line-delimited,
one message per line, so an oversized transcript keeps whole messages from the
end backward until the budget runs out, keeps the first message too when room
remains, and names what it dropped (`N message(s) omitted from the middle of
the transcript`) instead of silently shaving whatever byte the cap happened to
land on. Only a single message too large to fit the budget on its own falls
back to a plain byte cut of that message.

### Asking the judge more than once

A model asked whether an assertion holds does not answer the same way every
time. An assertion whose material sits near the edge of the judge's decision
flips between passes for reasons that have nothing to do with the run, and a
single opinion cannot tell that assertion apart from one the judge is sure
about: both arrive as a flat `passed` value.

`--grader-votes N` takes `N` opinions on every assertion the judge decides and
reports the majority, and writes the split into the result's `votes`. A divided
panel is a statement about the assertion rather than about the run: it says the
material admits both readings, which is a reason to write the assertion more
precisely. The evidence shown is evidence for the answer reported, never for the
side that lost.

`N` must be odd, because a panel that ties has decided nothing, and it costs one
judge request per vote per LLM-graded assertion. Mechanical, declarative, and
script graders are unaffected: they answer the same way every time, so there is
nothing for a second opinion to settle.

### Holding a run to a reference output

A `baseline` grader asks a comparative question the other graders cannot: not
whether the run cleared some property, but whether it is *at least as good as*
an output already accepted. `reference` is a relative path inside the skill
directory, resolved the way `schema_validation` resolves its schema, and
`criterion` says on what footing the two are being compared.

```json
{ "type": "baseline", "reference": "evals/golden/summary.md", "criterion": "covers every column in the input" }
```

Three properties are worth knowing before writing one.

**The judge is not told which output is the reference.** The two are shown as A
and B, and which label the run gets is fixed by the case id and the criterion.
A judge that knows which side is the incumbent answers a different question from
the one the case wrote down, and the position the run is shown in is itself a
bias, so the assignment varies between criteria and stays put across re-grades
of the same case.

**A tie passes.** "At least as good as" is the whole of what a baseline asks, so
only a run the judge places behind the reference has failed it. This is what
makes a baseline usable as a regression gate on the `old_skill` arm, where the
expected outcome is that nothing got worse rather than that everything improved.

**A reference that is missing or blank is a defect in the case.** It is reported
as a failure to grade, the same way `schema_validation` reports a broken schema,
rather than as a failed run. A blank reference in particular would be cleared by
any output at all, which is the one answer a comparison must never give by
accident.

The same line holds from the other side: a run whose target is empty or missing
falls short of its baseline without a judge being asked. Nothing is not at least
as good as something, and a comparison between a reference and an empty string is
one a judge can only answer by guessing.

Both sides are held to the same share of the judge payload. A long run does not
get to push the reference out of the request, because a comparison against a
clipped reference is a comparison between two different things.

A baseline costs judge requests, `--grader-votes` of them, exactly as an `llm`
grader does. Under `--grader script` the reference is handed to the script in
the payload as `baseline`, so the script can answer the comparison rather than
grading the run on its own.

### Schema validation

A `schema_validation` grader, or the equivalent prose form (`"<target> validates
against schema <name>"`), is answering a question about the target: whether it
conforms to the named schema. A schema file that is missing, unreadable, or not
itself a valid JSON Schema document is a defect in the case, not in the run
being graded, so it is reported the same way any other malformed suite is: as a
failure to grade at all, rather than as a failed assertion. The same rule holds
for the prose form when it names no schema file; it does not fall back to
checking that the target merely parses as JSON, since that would silently grade
something other than what was written.

---

## Artifact: `report.json`

**Status: available.** Always written by `eval run`.

This file is a **superset** of the
agentskills.io report model; companion artifacts (`benchmark.json`,
`grading.json`, etc.) follow the docs shape. See
[Divergences from agentskills.io](../explanation/divergences-from-agentskills-io.md)
for intentional differences and the backward-compatibility contract (fixture
snapshot tests under `crates/trg/src/agentskills/testdata/reports/`).

### Top-level fields

| Field | Type | Description |
| ----- | ---- | ----------- |
| `report` | object | Report metadata (id, timestamp, producer, optional CI context) |
| `suite` | object | Skill and eval-suite hashes |
| `dimensions` | object | Eval cases, assertions, scenarios, model configs, skill revisions, grading strategies |
| `runs` | array | One record per (eval case × scenario) |
| `assertion_results` | array | Per-assertion grading outcomes |
| `summaries` | object | Aggregated counts by scenario |
| `comparisons` | array | Cross-scenario comparison records |
| `budget` | object | Present whenever the pass was given a runner, including one whose every run was served from cache and so spent nothing. Absent only when the pass had no runner at all. What the pass spent, and against what ceiling. See [Bounding what a pass may spend](#bounding-what-a-pass-may-spend) |

`assertion_results` is populated by `eval grade`, which flattens every run's
`grading.json` into it, adding `run_id` and `eval_case_id` and carrying every
other field forward as declared there, including `name`, `excluded`,
`rationale` and `votes` where present. `comparisons` is populated by
`eval compare`. Both are empty until those subcommands run.

### `budget` section

| Field | Type | Description |
| ----- | ---- | ----------- |
| `ceiling_usd` | number | Value of `--max-cost-usd`, always greater than zero. Absent when the pass ran with no ceiling |
| `spent` | object | What the pass spent, or why nobody can say. `{"kind": "priced", "usd": N}` when the runner publishes a price for a run, totalling every runner invocation in the pass including attempts `--retries` discarded. `{"kind": "unpriced", "harness": "codex"}` when it publishes none, since a zero there would report a free pass rather than an unpriced one |
| `exhausted` | bool | Whether spend had reached the ceiling by the time the pass finished |
| `runs_skipped` | integer | Runs not started because the ledger had already refused them |

### `report` section

| Field | Type | Description |
| ----- | ---- | ----------- |
| `id` | string | Report identifier (matches directory name) |
| `generated_at` | string | RFC 3339 timestamp |
| `producer.name` | string | Always `trg` |
| `producer.version` | string | `trg` crate version |
| `environment` | string | Environment policy the runs were executed under: `scrubbed`, `isolated`, or `inherited` |
| `permission` | string | Permission grant the runs were executed under: `workspace_write` or `unrestricted`. Absent in reports written before this field existed, which is equivalent to `workspace_write`. See [Run permission](#run-permission) |
| `ci` | object | Present when running inside GitHub Actions (`GITHUB_ACTIONS=true`) |

### `suite` section

| Field | Type | Description |
| ----- | ---- | ----------- |
| `skill_name` | string | From skill frontmatter |
| `skill_path` | string | User-supplied `--skill-dir` path |
| `skill_hash` | string | `sha256:` digest of `SKILL.md` |
| `eval_dir` | string | Directory, relative to `skill_path`, the suite was resolved from (`--eval-dir` or its default `evals`). Absent in reports written before this field existed, which is equivalent to `evals`. Read back by `grade` and `next-iteration` so they resolve the same suite without a flag of their own |
| `evals_path` | string | `<skill_path>/<eval_dir>/evals.json` for a manifest suite, `<skill_path>/<eval_dir>` for a suite authored as case directories |
| `evals_hash` | string | `sha256:` digest of `evals.json` for a manifest suite, or of a canonical walk of every case's files for a suite authored as case directories, under a distinct regime tag so the two can never collide |
| `case_selection` | object | Present only when the run covered part of the suite. See [Covering part of a suite](#covering-part-of-a-suite) |

### `runs[]` record

| Field | Type | Description |
| ----- | ---- | ----------- |
| `id` | string | e.g. `run-001` |
| `eval_case_id` | string | References an eval case id |
| `scenario_id` | enum | `with_skill`, `without_skill`, or `old_skill` |
| `model_config_id` | string | Value of `--model-config` |
| `skill_revision_id` | string | Always `current` today |
| `attempt` | integer | Which draw of the cell this run is, `1..N` for `--attempts N` |
| `status` | string | `skipped`, `completed`, or `failed` |
| `paths.workspace` | string | Relative path to the run workspace |
| `artifacts` | array | Artifact descriptors (transcript when runner completes, `mock_calls` when the case declares mcp mocks) |
| `metrics` | object | `duration_ms`, token counts, and `cost` (populated by runner). See [Artifact: `timing.json`](#artifact-timingjson) for what `total_tokens` and `cached_tokens` mean, and [What a run cost](#what-a-run-cost) for `cost` |
| `skill_integrity` | object | Tamper detection result (when runner used) |
| `read_only_fixture_violations` | string[] | Paths of read-only fixtures whose staged copy no longer matched its source after the run (when runner used). See [Read-only fixtures](#read-only-fixtures) |
| `warnings` | string[] | Run-level anomalies that do not fail the run on their own, such as a read-only fixture that changed, an expected output that was never written, a path reached outside the workspace, or a usage field the harness wrote in a shape that is not a token count. Also carries the reason a run that was never started was skipped |
| `case_score` | float or null | This run's own pass rate over its scored assertions, from grading. `null` until graded, or when grading scored nothing for this run. A suite-wide pass rate can stay high while one run's `case_score` is low; check both |
| `mock_violations` | array | Every logged `expect` mismatch from `mock-calls.jsonl`, read back after the run finished. Empty when the case declares no mocks or violates nothing |

Run ordering: eval cases in manifest order, then scenarios in flag order.

---

## Artifact: `grading.json`

**Status: available.** Written by `eval grade` (and by `eval run --grade`) as
`runs/<run-id>/grading.json`. `verify` discovers these recursively under a
workspace tree.

`unsupported` narrows `pass_rate` to scored results only. `pass_rate` is
nullable, because a run where nothing could be scored has no pass rate and
reporting `0.0` reads as a total failure. `excluded` takes an arm-scoped grader
out of the score in both arms. `ungraded` marks an assertion no mechanical
pattern recognized and no LLM judge was consulted for; it is neither a pass nor
a fail, because nothing ever attempted it, and it stays out of `pass_rate` for
the same reason `unsupported` does. `votes` is present only when a panel of
judges decided the result. `weight` is present only when the declaring grader
gave one; a case whose graders left every weight undeclared reports the same
`pass_rate` it always has.

```json
{
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
    },
    {
      "assertion": "the skill was engaged",
      "passed": true,
      "evidence": "the run read the staged skill directory",
      "grader": { "kind": "declarative" },
      "excluded": "'skill_used' is settled by whether the skill was staged rather than by the run, so it is reported in both arms and scored in neither; declare 'arm': 'both' to score it anyway"
    },
    {
      "assertion": "the report reads as encouraging to a first-time user",
      "passed": false,
      "evidence": "no mechanical pattern recognized this assertion and no LLM judge was resolved for this run",
      "grader": { "kind": "needs_llm" },
      "ungraded": "no mechanical pattern recognized this assertion and no LLM judge was resolved for this run"
    }
  ],
  "summary": {
    "passed": 1,
    "failed": 0,
    "unsupported": 1,
    "excluded": 1,
    "ungraded": 1,
    "total": 4,
    "pass_rate": 1.0
  }
}
```

| Field | Type | Notes |
| ----- | ---- | ----- |
| `assertion_results[].assertion` | string | Non-empty. Accepts `text` as an alias. For a typed grader, its rendered description |
| `assertion_results[].passed` | bool | Pass/fail for this assertion. Always `false` when `unsupported` or `ungraded` is present. An indicator rather than a score when `excluded` is present |
| `assertion_results[].evidence` | string | Non-empty. A passing result must not merely restate its assertion |
| `assertion_results[].grader.kind` | enum | `mechanical`, `declarative`, `llm`, `script`, `needs_llm`, or `none` |
| `assertion_results[].name` | string | Present when the grader declared a `name` |
| `assertion_results[].rationale` | string | Optional judge reasoning |
| `assertion_results[].unsupported` | string | Present when the runner cannot answer this check. Why it could not be graded |
| `assertion_results[].excluded` | string | Present when the grader presupposes the skill. Why it is reported rather than scored |
| `assertion_results[].ungraded` | string | Present when no mechanical pattern recognized the assertion and no LLM judge was consulted. Why nothing attempted it |
| `assertion_results[].votes` | object | Present only under `--grader-votes N` with `N` above 1. `{passed, failed}` opinions behind this result. See [Asking the judge more than once](#asking-the-judge-more-than-once) |
| `assertion_results[].weight` | number | Present when the grader declared one. Must be greater than zero |
| `summary.passed` | integer | Must equal the count of scored, passing results |
| `summary.failed` | integer | Must equal the count of scored, failing results |
| `summary.unsupported` | integer | Must equal the count of results carrying `unsupported` and not `excluded` |
| `summary.excluded` | integer | Must equal the count of results carrying `excluded` |
| `summary.ungraded` | integer | Must equal the count of results carrying `ungraded` |
| `summary.total` | integer | Must equal `assertion_results` length |
| `summary.pass_rate` | float or null | Must equal `passed / (total - unsupported - excluded - ungraded)`, or `null` when nothing was scored |

An ungraded assertion forces `eval grade` to exit non-zero, in every mode and
regardless of `--strict`, because a suite that measured less than it declared
is not a passing suite just because nothing it did measure failed. The exit
lists how many assertions went ungraded and which ones, capped, so the count is
never the only thing an operator has to act on. `eval verify` and `eval ci`
carry the same refusal: a non-zero `ungraded` count is reported as a violation
of its own, so a `--min-pass-rate` gate cannot read a partially-measured suite
as a clean pass.

---

## Artifact: `timing.json`

**Status: available.** Written by agent runners (`cursor-agent`, `claude-code`,
`codex`) alongside each run when `--runner` is set.

Location: `runs/<run-id>/timing.json` (sibling of `workspace/`).

```json
{
  "duration_ms": 1234,
  "total_tokens": 150,
  "input_tokens": 100,
  "output_tokens": 50,
  "cached_tokens": { "read_tokens": 10 },
  "cost": { "kind": "priced", "usd": 0.0123 },
  "cost_usd": 0.0123
}
```

| Field | Type | Required | Notes |
| ----- | ---- | -------- | ----- |
| `duration_ms` | integer | yes | Must be > 0 |
| `total_tokens` | integer | no | When present, must be > 0. Always `input_tokens + output_tokens`, the one definition every runner agrees on, so a claude-code run and a cursor-agent run are comparable without knowing which harness produced either. Never includes `cached_tokens`, because runners disagree on whether a cached token is billed on top of `input_tokens` or already counted inside it |
| `cached_tokens.read_tokens` | integer | no | Tokens served from a cached entry, billed at a discount. Absent when this runner's harness never reports cache reads, not when it reported reading zero |
| `cached_tokens.write_tokens` | integer | no | Tokens spent writing a fresh entry into the cache, billed at a premium. Absent when this runner's harness never reports cache writes, not when it reported writing zero |
| `cost` | object | no | What the run cost, or why nobody can say. See [What a run cost](#what-a-run-cost) |
| `cost_usd` | float | no | The price alone, for readers written before `cost` existed. Present for exactly the runs `cost` reports as `priced` |

`cached_tokens` itself is absent when the harness reports no cache activity at
all, which is a different claim from a harness that checked and cached
nothing. Either side may be absent on its own: a harness that names one and not
the other is recorded as having measured only the side it named.

Any count is absent when the harness reported nothing for it: no usage block at
all, or a block that did not name the field. A field the harness did name but
filled with something no reader can add up (a string, a fraction, a negative, an
explicit `null`, an object) leaves its count absent here too, and the run reports
it in `report.json` under `runs[].warnings`, naming the harness, the field, and
the value found. `timing.json` gains no field for it: the count is genuinely
absent, which is what every reader of this file already handles, while the fact
that the harness contradicted its own record is a run-level anomaly and is read
where a run's other anomalies are read.

Token counts and duration are also copied into `report.json` run metrics after
the runner completes.

### What a run cost

Only `claude-code` prices a run. A run of a harness that prices nothing is not a
run that cost nothing, and an absent number could not tell the two apart, so a
reader totalling a mixed history added the silent harness in as a zero and
reported it as the cheap one.

`cost` says which of the two happened:

| Shape | Means |
| ----- | ----- |
| `{ "kind": "priced", "usd": 0.0123 }` | The harness priced this run, and this is what it came to |
| `{ "kind": "unpriced", "harness": "codex" }` | This harness prices no run at all, so there is nothing to total and nothing that was free |
| absent | The harness prices its runs and did not price this one |

A run that failed or hit its timeout is recorded the same way, because what its
harness prices did not change when the run did not finish.

`cost_usd` keeps carrying the bare number for readers written before `cost`
existed, and is present for exactly the runs `cost` reports as `priced`. The
same pair appears in `report.json` under `runs[].metrics`; a run recorded before
`cost` existed carries only `cost_usd`, and is read back as a priced run,
because only a harness that prices its runs ever wrote a number there.

---

## Artifact: `benchmark.json`

**Status: available.** Written by `eval benchmark` (and by `eval run
--benchmark`), aggregating the grading and timing artifacts of a report bundle
into per-scenario duration, token, and cost summaries.

A bucket's `tokens.cost` says what its total covers, because a total over some
of a bucket's runs is not the bucket's cost:

| Shape | Means |
| ----- | ----- |
| `{ "kind": "whole", "usd": 1.0 }` | Every run in the bucket carried a price, so this is the bucket's cost |
| `{ "kind": "partial", "usd": 0.25, "runs": 1 }` | Only `runs` of the bucket's runs carried a price, and this is their total |
| `{ "kind": "unpriced", "harness": "codex" }` | At least one run came from a harness that prices nothing, so the bucket has no total |
| absent | No run in the bucket reported anything about cost |

`tokens.cost_usd` is published only for a `whole` bucket, which is the only
total an earlier release's readers would have been right to read as the
bucket's cost. For the same reason a scenario delta reports `cost_usd` only when
both arms priced every run behind them: a difference between two totals covering
different numbers of runs is not a cost difference.

Each scenario delta publishes the interval around its pass-rate subtractions
next to them, so a difference drawn from a handful of runs is not read as a
finding. See
[How many times a cell is drawn](#how-many-times-a-cell-is-drawn).

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

## Artifact: `report.html`

**Status: available.** Written by `eval html-report` into the report directory,
alongside `report.json`.

```text
trg ai skills eval html-report <REPORT_DIR>
```

`REPORT_DIR` is the directory containing `report.json`, the same directory
every other `eval` subcommand reads and writes against.

The page is a single self-contained HTML file: every style is inlined, there is
no JavaScript, and nothing on the page references the network. No CDN script,
remote stylesheet or font, analytics beacon, or external image is ever emitted,
so the report opens correctly from a `file://` URL with no connectivity and
carries nothing out of the machine it was generated on. Links to output
artifacts stay inside the bundle and are never absolute URLs; they are
percent-encoded, so an artifact an agent named with a `#`, a `?`, or a space
still resolves to the file it names.

Because every value it renders (final text, transcript excerpts, output
artifacts, assertion evidence, judge rationales, skill names, file paths)
originates from an LLM agent under evaluation and must be treated as untrusted,
every interpolated string is HTML-escaped through a single chokepoint before it
reaches the page. Nothing is written into the output outside that path.

The page covers bundle identity and provenance (skill, harness, scenario,
timestamps, attempt counts, and the skill integrity report), per-scenario
summaries, and a case-by-case, arm-by-arm breakdown of every run: pass or fail,
each assertion's evidence, and non-scoring outcomes (`unsupported`, `excluded`)
shown distinctly from a scored result rather than folded into a pass or fail.

---

## Covering part of a suite

A full pass costs a model call per case, per scenario, per attempt, so a suite that grows
past a handful of cases stops being something to run while iterating on one of them.
`--case` and `--tag` narrow what a single invocation covers:

```shell
# One case, by id
trg ai skills eval run --skill-dir ./skills/csv-analyzer --out-dir ./artifacts \
    --case analyze-sales

# Every case whose id starts with analyze-, and every case tagged smoke
trg ai skills eval run --skill-dir ./skills/csv-analyzer --out-dir ./artifacts \
    --case 'analyze-*' --tag smoke
```

`--case` takes a glob, not a regular expression: `*` stands for any run of characters, `?`
for exactly one, and every other character is matched literally. The pattern is matched
against the whole id, so `--case analyze-sales` does not select `analyze-sales-by-region`.

Each flag narrows the selection and repeating one widens it. Two `--tag` flags cover a
case carrying either tag; a `--case` and a `--tag` together cover only the cases that both
match the pattern and carry the tag.

A selection matching none of the suite's cases is refused rather than run. An eval that
covers nothing would otherwise exit successfully with an empty report, which reads the
same as a suite that found no problems.

A narrowed run records what it covered under `suite.case_selection` in `report.json`:

```json
{
  "suite": {
    "evals_hash": "sha256:...",
    "case_selection": {
      "cases": ["analyze-*"],
      "tags": ["smoke"],
      "covered": 2,
      "declared": ["analyze-refunds", "analyze-sales", "summarize-quarter"]
    }
  }
}
```

`evals_hash` covers the whole manifest either way, so without this field a narrowed run
and a full one are indistinguishable to anyone comparing two reports. The field is absent
when the run covered every case the suite declares, however the selection was written: a
pattern that happens to match the whole suite narrowed nothing.

`declared` names the whole suite the selection was taken from, since `dimensions.eval_cases`
lists only what the run covered. Suite drift is diffed against `declared`, so a case a run
skipped is not reported as one the suite lost, nor as one it gained the next time a run
covers it.

## Measuring triggering

A prompt that names the skill measures how well a run uses a skill it was
handed. It cannot also measure whether the skill's own `description` wins the
run's routing decision, because the prompt already made that decision. Those
are two different questions, and `skill_disclosure` says which one a case asks.

| `skill_disclosure` | Prompt | Skill staged at | `companion_skills` |
| ------------------ | ------ | --------------- | ------------------ |
| `announced` (default) | Names the staged directory, the skill `name`, and its `description` | `.skill/`, or `.old-skill/` in the `old_skill` arm | Refused when the suite loads |
| `unannounced` | Says nothing about the skill | `skills/<skill-name>/` | Staged as siblings under `skills/` |

An unannounced case gets the same prompt in both arms, so the only difference
between them is whether the skill is in the workspace. It is staged under a
plain directory rather than the dot-prefixed link because a run that was told
nothing can only find what a listing of its workspace reports.

State the expectation with `skill_used`, which answers on every runner. A
`tool_used` grader naming one harness's skill-invocation tool cannot: `codex`
and `cursor-agent` have no such tool, so the same case would be unanswerable in
two of three columns. What trg observes is a native skill tool call or a tool
call that names the staged directory. A path that left the workspace is never
counted, so reading a skill the harness installed elsewhere does not pass.

What this does not do is register the staged skill with a harness's own skill
discovery mechanism. A harness that has one would offer the skill from its own
system prompt, which is a stronger measurement than discovery from the working
tree. trg stages the same way for all three runners instead, so the number
means the same thing in every column.

`eval run` and `eval verify` both warn when a case checks `skill_used` while
announcing the skill, because such a case cannot pass or fail for the reason it
was written. The `init` scaffold's `triggers-the-skill` case is unannounced for
that reason, and its prompt is a placeholder: replace it with the words a user
would actually use, naming neither the skill nor where it lives.

### Skills staged only to be passed over

An unannounced case asks whether the run reached for the skill on its own. A
workspace holding exactly one skill answers that for it: there was nothing else
to reach for, and a skill that won cannot be told apart from a skill that was
the only option on offer. `companion_skills` names other skill directories to
stage beside the one under test, so the decision is the run's own.

```json
{
  "id": "reaches-for-the-skill",
  "prompt": "I have a quarterly sales export in evals/files/sales.csv and I need the monthly revenue picture out of it.",
  "expected_output": "The run locates the skill on its own and follows it rather than improvising.",
  "files": ["evals/files/sales.csv"],
  "skill_disclosure": "unannounced",
  "companion_skills": ["evals/companions/pdf-forms", "evals/companions/release-notes"],
  "graders": [{ "type": "skill_used" }]
}
```

Each path is relative to the skill directory, the way `files` and `scaffold`
are, and must be a skill directory in its own right: a `SKILL.md` carrying a
`name` and a `description`, since the description is what a run routes on. Each
one stages at `skills/<its own name>/`, a sibling of the skill under test, so a
listing of the workspace reports them as peers and no prompt has to change.

- Only an unannounced case may declare them. An announced prompt names the
  skill and the directory it sits in, so it has already made the routing
  decision a distractor exists to leave open. A companion on an announced case
  is refused by name when the suite is read, before a run is spent on it.
- Two companions carrying the same skill `name` would stage as one directory,
  handing the run fewer skills to choose between than the case declares. That
  is refused, naming both.
- A companion carrying the skill under test's own `name` would replace the
  skill being measured, and is refused for the same reason. The `old_skill`
  arm stages its revision under that revision's own name, which
  `--allow-skill-name-mismatch` lets differ, so a companion wearing the older
  name is refused in that arm too rather than staged over the revision the arm
  was drawn to measure.
- `--skill-staging` decides how they land, `symlink` or `copy`, exactly as it
  does for the skill under test, and each companion's own eval suite directory
  is withheld from the workspace the same way.
- Every arm stages them, the `without_skill` arm included. The arms are meant
  to differ in the skill under test and nothing else, so staging distractors in
  only one of them would make the gap between the arms partly the gap between a
  populated workspace and an empty one.
- A companion is an input to the run. Editing one changes the case's cache key,
  so the next pass draws the case again rather than serving the grade from
  before the edit.

---

## The state a case is asking about

A run starts in an empty workspace, so by default every case is a cold first turn
on a blank slate. A skill whose work depends on the state of a directory cannot be
asked about that work there: a case about a repository mid-rebase, a project with a
lockfile, or a file that is already wrong has no way to say so.

A case says so with `scaffold`, a path to a script inside the skill directory:

```json
{
  "id": "resolves-a-conflicted-rebase",
  "prompt": "Finish the rebase.",
  "expected_output": "The rebase is completed and the conflict is resolved.",
  "scaffold": "evals/scaffolds/conflicted-rebase.sh",
  "graders": [{ "type": "tool_used", "tool": "Bash", "input_match": "git rebase" }]
}
```

The script runs with the workspace as its working directory, before the skill and
the case's `files` are staged, so what a case declares in `files` survives what its
scaffold wrote to the same path. It must be executable; it is spawned directly, so
its shebang decides what interprets it.

Every attempt starts from an emptied workspace, so a retry after a runner failure
re-runs the script on the directory the case declared rather than on what the last
attempt left behind. The script does not have to be idempotent.

trg runs the script itself rather than asking a harness to, which is what makes the
case portable: the same case puts the same directory in front of `claude-code`,
`codex` and `cursor-agent`, and none of them needs a setup mechanism of its own.

### `--allow-scaffold` is required

The script is author-supplied code that runs with the operator's own reach, so
nothing runs it on the strength of a manifest alone. Without `--allow-scaffold` a
case that declares a scaffold **fails its runs**, naming the script and the flag.

It fails rather than running on a blank slate because a case scored against a
workspace it never asked for reads as an answer about the skill when it is an answer
about the wrong directory. Only that case's runs fail, so the rest of the suite is
still measured.

A pass with no `--runner` neither runs the scaffold nor needs the flag, because its
runs are scaffolded as `skipped` and no agent starts in the workspace.

A case's scaffold is part of what identifies its runs, so editing the script
re-executes rather than serving a cached run, under `--no-cache` and under
`--reuse-completed` alike.

### Seeding a conversation

A case may declare `conversation_history`, a relative path to a transcript inside
the skill directory, to ask about a mid-conversation turn rather than a first one.
Resuming a transcript is a per-harness mechanism, and none of `claude-code`,
`codex` or `cursor-agent` offers one that adopts an arbitrary, case-authored
transcript as history it did not itself produce: each only resumes a session it
already recorded itself, and claude's `--input-format stream-json` re-runs every
scripted turn as a live model call rather than replaying it. See `conversation
seeding` in the harness support table above.

A case that declares `conversation_history` is skipped rather than run, naming
the case and the reason, on every harness. It is skipped rather than run against
a fresh conversation because a field that quietly answered turn one on every
harness would misreport the case's own precondition, and a suite whose results
depended on that silent substitution would be exactly the kind of result trg
exists to keep from happening.

Such a run is recorded with `status: skipped` and `failure_kind: unsupported`,
and `grade` passes over it for the same reason it passes over a run the cost
ceiling refused: nothing was asked of the runner, so there is no workspace or
transcript to read and no pass rate to charge the case with.

---

## Choosing which scenarios run

Naming `--scenario` one or more times runs exactly those scenarios and nothing
else. Naming it zero times runs `with_skill` and `without_skill` together: a
`with_skill` pass alone can only say whether a case was covered, not whether the
skill changed anything, because the report has no baseline in it to compare
against. An eval suite exists to answer that question, so a pass that named no
preference gets both arms.

The one exception is `--reuse-completed`. Reuse serves a prior completed run
for the same case and scenario (see [Reusing a completed run](#reusing-a-completed-run)),
which is inherently single-arm, so defaulting to both arms under it would turn
a command that used to run one arm into one that always fails. Naming no
`--scenario` under `--reuse-completed` runs `with_skill` alone. Naming more
than one `--scenario` together with `--reuse-completed` is still rejected: see
[Reusing a completed run](#reusing-a-completed-run).

Running both arms by default doubles the runs of a pass that also leaves
`--attempts` at its default of `3`, from three runs per case to six. Pair
`--max-cost-usd` with a default pass to keep that bounded; see
[Bounding what a pass may spend](#bounding-what-a-pass-may-spend).

---

## Scenario kinds

| Kind | CLI value | Runner behavior |
| ---- | --------- | --------------- |
| With skill | `with_skill` | Stages skill to `.skill/` in workspace; prompt prefixed with skill frontmatter. An unannounced case stages to `skills/<skill-name>/` and prefixes nothing |
| Without skill | `without_skill` | Raw eval prompt; the skill under test is not staged |
| Old skill | `old_skill` | Stages the `--old-skill-dir` revision to `.old-skill/` in the workspace; prompt prefixed with that revision's frontmatter |

A case's `companion_skills` are staged in every kind, the one that stages no
skill included, so the arms differ in the skill under test alone. See
[Skills staged only to be passed over](#skills-staged-only-to-be-passed-over).

### The eval suite is withheld from the workspace

The staged directory holds the skill under test minus its top-level eval suite
directory (`evals/` by default, or whatever `--eval-dir` resolved to). The
suite is the answer key: it carries each case's
`expected_output`, its natural-language assertions, and its graders' literal
`contains` text and `regex` patterns. A run that could read it could be scored
on text it copied rather than work it did, and the with-skill prompt points the
agent straight at `.skill/`, so the suite is staged in neither
`--skill-staging copy` nor `--skill-staging symlink`.

This costs a case nothing. The fixtures a case names in `files` are staged
separately into the workspace root, and they are the only part of `evals/` a
run is meant to see. Only the top level is filtered, so a nested `evals/`
deeper in the skill tree is treated as the skill's own content and staged
normally.

### Read-only fixtures

A bare string in `files` names a fixture the agent is free to change, which is
what a case about editing a file needs. A case about reading one needs the
opposite: the fixture has to still be the thing the case's `expected_output`
and graders describe after the run, not whatever the agent left behind.

```json
{ "files": ["evals/files/input.csv", { "path": "evals/files/reference.csv", "mode": "read_only" }] }
```

The object form names the same relative path as the bare string and adds
`mode`, `writable` (the default, and what a bare string means) or
`read_only`. Nothing about an existing suite's fixtures changes: every bare
string still parses, still means writable, and a fixture authored as a bare
string is written back as one, never rewritten into the object form.

On Unix, a read-only fixture is staged with its write bits cleared, so an
agent that tries to edit it in place is refused by the filesystem before it
gets the chance. That is a courtesy, not the guarantee: an agent can still
delete the file and write a fresh one in its place, which touches no
permission bit. What actually holds a read-only fixture to its word is a
content hash taken before the run and compared against the same fixture after
it, the same comparison `skill_integrity` makes of the skill directory. A
fixture's containing directory is left writable regardless of the fixture's
own mode, because removing an entry needs write permission on the directory
that holds it, not on the entry itself, and the workspace has to be
removable between attempts.

A run whose read-only fixture changed, however it changed, is reported as a
failing assertion naming the fixture's path, so the operator sees which
fixture and not just a count, and the case cannot be read as passing on the
strength of assertions that never looked at the fixture at all.

A top-level version control directory (`.git`, `.jj`, `.hg`, `.svn`) is
withheld for the same reason. When the skill is its own checkout, its history
holds every revision of the suite, so a run handed the working tree without
`evals/` could ask version control for the answer key instead. No run needs a
skill's history to do its work.

Copying dereferences links, which is what makes the staged copy self-contained,
so a link inside the skill decides what ends up in the workspace. A link that
resolves into the withheld suite, or out of the skill altogether, is left out
the same way the suite is, rather than delivering its target into the run's own
directory with no link left to give it away. A skill that keeps content behind
such a link is staged without it under `--skill-staging copy`.

Leaving the suite out of the staged directory keeps it out of a listing, which
is all a symlink can offer. A symlink names the path it points at, so a run that
reads one learns where the skill really lives, and the suite it was not given is
one directory over. `--skill-staging copy` is the default for that reason: every
path a run can follow out of a copied `.skill/` stays inside the workspace.

`--skill-staging symlink` remains available for a skill large enough that copying
it per run costs real time, at the price of disclosing the skill's location to
any run that looks. Because the filter has to skip an entry, it stages `.skill/`
as a real directory holding one symlink per entry rather than as a single symlink
to the skill root.

Choosing it prints a warning to stderr naming what the resulting score does not
rule out, so a report produced under symlink staging is not mistaken for one
whose runs could not reach the answer key. Every run records the mode it was
staged under as `skill_staging` in `report.json`.

`--scenario old_skill` requires `--old-skill-dir`. The old skill must carry the
same `name` as the current one unless you pass `--allow-skill-name-mismatch`,
which guards against comparing two unrelated skills by accident. Tampering
detection is scoped to the old skill directory for these runs.

---

## Run environment

A harness subprocess inherits nothing by accident. `--environment` chooses how
much of the host machine a run can see.

| Policy | Environment | `HOME` | Harness config home |
| ------ | ----------- | ------ | ------------------- |
| `scrubbed` (default) | Replaced with an allowlist | Host | Host |
| `isolated` | Replaced with an allowlist | Per run | Per run |
| `inherited` | Passed through untouched | Host | Host |

The allowlist is fixed: the variables a CLI needs to start (`PATH`, `TMPDIR`,
`HOME`, locale and terminal settings), the variables that decide whether it can
reach the network (proxies and their trust stores), the cloud credential
variables, the credential variables for the harness being run, and the variable
that names the harness config home. Everything else is dropped.

The config home variable is on the list because `scrubbed` leaves the config
home alone. Dropping it would not leave it alone: the harness would fall back to
the default under `HOME`, which is neither the operator's config home nor one
this run set up.

Scrubbing is not only about secrets. Launching `trg` from inside an agent
session puts that session's own identity in the environment, including a live IPC
socket and token, and passing those to a run hands it a channel back into the
session that launched it. Project variables are dropped for the same reason a
fixture is declared rather than assumed: a run that depends on one is not
reproducible anywhere else.

Each harness receives only its own credential variables, so a `codex` run cannot
read an Anthropic key and a `claude-code` run cannot read an OpenAI one.

### Variables a case sets

A case whose input cannot be expressed as a file in the workspace declares it as
`env`:

```json
{
  "id": "honours-the-configured-region",
  "prompt": "Deploy the stack.",
  "expected_output": "Deployed to eu-west-1.",
  "env": { "EVAL_REGION": "eu-west-1" }
}
```

Every name has to start with `EVAL_` and hold only `A-Z`, `0-9` and underscore.
A suite that names anything else is rejected while it is read, before a run is
started.

The prefix is what keeps this an addition to the environment rather than an edit
of it. `PATH` decides which binary the harness is, `HOME` decides where it finds
its config, and a credential variable decides whose account pays. A case that
could name those would be rewriting the isolation the policy just promised, and
would do it invisibly, since nothing downstream tells a value a case set apart
from one the allowlist admitted.

The variables reach the run under every policy, `inherited` included. They are
recorded in the run's `env.json` under the same rule as everything else there: a
name that reads as a secret is listed without its value.

### Steering a case's system prompt

A case whose question is about how the agent was instructed, rather than about
what it was asked, declares `append_system_prompt`:

```json
{
  "id": "keeps-the-house-spelling",
  "prompt": "Summarise the release.",
  "expected_output": "A summary in British English.",
  "append_system_prompt": "Answer in British English."
}
```

The text is appended to whatever system prompt the harness assembles for itself,
and reaches it exactly as written. A blank one is rejected while the suite is
read: it asks for the case to be steered and then steers it nowhere.

Appending to the system prompt is a per-harness mechanism, and only `claude-code`
offers one. See `system prompt append` in the harness support table above. On
`codex` and `cursor-agent` the case is skipped, recorded with `status: skipped`
and `failure_kind: unsupported`, rather than run without the appendix: a run
missing the instruction it was supposed to carry is not a weaker answer to the
case's question, it is an answer to a different one.

### `--environment isolated`

The harness config home is where a harness keeps the per-user state that changes
what its agent does: installed skills, global instruction files, MCP servers,
permission settings, and history. Under `scrubbed` that state is still live, so
a skill the operator happens to have installed globally is present in *both*
arms of a comparison, and the baseline is not a baseline.

`isolated` moves it aside. The run gets `HOME` inside its own run directory, and
the harness config home is redirected into it:

| Harness | Redirected by |
| ------- | ------------- |
| `claude-code` | `CLAUDE_CONFIG_DIR` |
| `codex` | `CODEX_HOME` |
| `cursor-agent` | `HOME` only; the CLI has no config home variable |

Authentication has to survive the redirect, so the auth files from the host
config home are carried into the run's config home, and nothing beside them is.
A file that holds credentials and nothing else is symlinked rather than copied,
so no credential is duplicated onto disk and a token refresh still reaches the
real file.

`cursor-agent` keeps its login in `cli-config.json`, alongside its permissions,
approval mode, and model selection, which are exactly what an isolated run holds
still. That file is therefore reduced rather than symlinked: only the members
that carry the login are written into the run's config home. Anything
unrecognized is left behind, so a harness that moves its login elsewhere fails
to authenticate rather than quietly handing the run the operator's settings
again.

That is not always sufficient. A harness may derive the identity of its
credential store from the config home path, in which case redirecting the path
invalidates a working login. Where the harness exposes a variable that locates
credentials independently, an isolated run pins it to the host config home;
`claude-code` is pinned through `CLAUDE_SECURESTORAGE_CONFIG_DIR`. Where it does
not, an isolated run needs credentials reachable from the environment, which is
the normal case in CI and the reason `isolated` is the right policy there.

`--environment inherited` is the escape hatch for a machine where neither route
works. It reproduces the behaviour of releases before this flag existed, and
because it is recorded in `report.json` a reader can tell that a comparison was
made against an unknown baseline.

Every policy writes what a run actually received to `env.json` in the run
directory: the variables, with secret-looking values left out, and a
`config_home` entry naming where the harness's config home was, whether it was
the operator's or one made for this run, and how it was found, a variable the
harness honours or a fallback under `HOME`. `cursor-agent` has no variable to
name its config home, so this is the only record of where it was.

The policy is part of a run's cache identity, alongside `--skill-staging`. A
completed run answers only for what it was allowed to see, so switching either
flag executes again rather than serving a run that saw something else.

---

## Run permission

A harness subprocess also needs to be told what it may do without asking, or
it falls back to whatever permission settings happen to be saved on the
operator's machine. `--permission` chooses the grant explicitly, so a run's
score never depends on a setting nobody wrote down.

| Grant | Meaning |
| ----- | ------- |
| `workspace_write` (default) | The run may write inside its workspace without prompting, and nothing wider |
| `unrestricted` | The run may act without prompting anywhere, including outside its workspace |

trg translates the grant into each harness's own flag:

| Grant | `claude-code` | `codex` | `cursor-agent` |
| ----- | ------------- | ------- | -------------- |
| `workspace_write` | `--permission-mode acceptEdits` | `-s workspace-write` | `--force` |
| `unrestricted` | `--permission-mode bypassPermissions` | `-s danger-full-access` | `--force` |

`cursor-agent` has only one documented non-interactive flag, so both grants
translate to `--force`; it draws no boundary between them for trg to translate.

trg always passes one of these two grants explicitly. There is deliberately no
mode that leaves the choice to the harness: a run whose permissions come from
the operator's machine is not a measurement of the skill, since the same suite
could then score differently on two machines.

---

## Running a skill from outside your working tree

A skill is prompts, and a pass runs them through a harness holding the
operator's own credentials. Evaluating a skill directory someone else wrote is
therefore closer to running their program than to reading their file, so a
directory that does not live under the working tree is asked about before
anything executes:

```text
skill directory '/tmp/downloaded/csv-analyzer' is outside this working tree, and
running it lets its author decide what an agent does as you; nothing here can
ask, so pass --trust-skill to say you have read it
```

The filesystem root is not a working tree for this purpose. A pass started there
would be claiming every skill on the machine as its own, which is not something
anyone says by changing directory, so such a pass is asked about the directory
like any other.

A directory under the working tree is never asked about. That is deliberate:
a checkout evaluating its own skills, which is what CI does, is the operator's
own tree by definition, so the gate is silent there rather than a flag every
pipeline has to learn.

The answer is remembered per directory in `trusted-skills.json` under
`$XDG_CONFIG_HOME/trg` (falling back to `~/.config/trg`), so a foreign
directory is asked about once rather than every pass. It is remembered by
directory and not by content: a directory whose contents change is still the
one you vouched for, and re-reading it when it changes is the operator's
business, the same as for any dependency.

Where there is nobody to ask, the run is refused rather than admitted.
`--trust-skill` is the way to say the reading already happened, which is what a
CI job pointed at a vendored third-party skill should pass. `--old-skill-dir`
is held to the same gate, since an `old_skill` arm executes its directory too.

---

## Harness support

Every runner drives a different CLI, and no two of those CLIs expose the same
controls. `no` in the table below is not a placeholder: it means the control is
absent from that harness's own `--help`, so trg has nothing to drive it with.

A cell without a qualifier names the mechanism trg's own invocation is built
from. A cell marked `(harness only)` names a control the harness offers that
trg does not exercise; it is recorded because it is a fact about the CLI worth
keeping visible, not because trg does anything with it.

The table is checked against the same declaration trg reads `support()` from,
so a test fails if the two ever disagree. That guarantees the doc matches the
declaration and nothing further.

A driven cell carries a second check, and how much that check is worth depends
on the mechanism. A driven flag is searched for in the argv the runner actually
builds, so promoting a cell to driven without wiring the flag fails the suite.
The two driven cells that are not flags, `run-scoped config home` and `cost
reporting`, are held against the neighbouring declarations they have to agree
with, `config_home()` and `pricing()`, because the variable is exported outside
argv construction and cost reporting is a fact about parsing the harness's own
output. Those two are consistency checks between declarations rather than
evidence that the export or the parse happens.

`mcp servers` is the one control the two harnesses that drive it reach through
different mechanisms. On `claude-code` it is a guarded flag rather than a plain
one: `--mcp-config` only takes effect alongside `--strict-mcp-config`, which
refuses to start if the config names a server the harness cannot reach, instead
of silently continuing without it. The check for a driven guarded flag covers
both halves: the argv trg builds must carry the value flag and its guard
together, never one without the other.

On `codex` it is not a flag at all. `codex` takes an MCP server table only as
the `config.toml` of the config home named by `CODEX_HOME`, and it publishes no
counterpart to `--strict-mcp-config`: a `-c` override merges into that config
rather than replacing it, so anything the config home already declared stays
live. Exclusivity therefore comes from owning the config home, which is why
that cell is only honoured under `--environment isolated`.

| Control | `claude-code` | `codex` | `cursor-agent` |
| ------- | ------------- | ------- | -------------- |
| tool allowlist | `--allowedTools` | no | no |
| turn cap | no | no | no |
| system prompt append | `--append-system-prompt` | no | no |
| mcp servers | `--mcp-config` (guarded by `--strict-mcp-config`) | `config.toml` in `$CODEX_HOME` | no |
| sandbox levels | `--permission-mode` | `-s` | `--force` |
| conversation resume | `--resume` (harness only) | `resume` subcommand (harness only) | `--resume` (harness only) |
| conversation seeding | no | no | no |
| run-scoped config home | `CLAUDE_CONFIG_DIR` | `CODEX_HOME` | no |
| cost reporting | reported | no | no |

`cursor-agent` accepts a sandbox level, but `--force` is its only documented
non-interactive grant: both of trg's permission grants collapse onto that one
flag, the same as in the "Run permission" table above.

---

## Reusing a completed run

A run is cached under everything that decides what it would do: the case, the
skill and suite digests, the fixture digest, the scenario, the attempt, the model
config, the runner and its version, the prompt contract, and the two policies
above. Change any of them and the run executes again. `--no-cache` skips the
lookup entirely.

`--reuse-completed` is the looser lookup, for an operator who wants each case
answered once while iterating rather than answered again for every model config
they try. It forgets the model config, the runner, and the runner version.

It does not forget the scenario. The arms of a comparison differ in the scenario
and nothing else, so a reuse that ignored it would answer the baseline with the
run that had the skill, and the report would show a delta of zero for a skill
that was never exercised. Each arm keeps its own reusable run, so reusing one
arm does not cost the other its entry.

It does not forget the attempt either. `--attempts N` asks for N draws of a cell
because one run of a non-deterministic agent says little about it, so serving the
second draw a copy of the first would report a spread of zero across attempts
that was never measured. Each draw is cached and reused under its own number, so
re-running the same command with the same `--attempts` still costs nothing.

`--reuse-completed` is rejected when more than one `--scenario` is requested in
a single invocation, for the same reason.

---

## How many times a cell is drawn

Every (case × scenario) cell is executed three times by default, and each draw is
its own run record with its own `attempt` number.

A case can pin its own count instead, with `attempts`, for a case whose stability
is the question or whose cost makes three draws too expensive:

```json
{ "id": "flaky-under-ambiguity", "prompt": "...", "expected_output": "...", "attempts": 5 }
```

`--attempts` overrides every count a case pinned, so a cheap smoke pass or a deep
one over a whole suite is still one flag. The flag is deliberately left unset
rather than defaulted to `3`: a count defaulted at the CLI cannot be told apart
from one an operator typed, and a case that pinned five draws would be silently
overridden by an operator who named nothing.

The default is not caution about flakiness, it is what makes the numbers
readable. An agent asked the same question twice does not answer it the same way
twice, so a single run of a case is one draw from a distribution rather than a
measurement of the skill. With one draw per cell, `benchmark.json` reports a
spread of zero because none was measured, a pass rate that moved between two
passes cannot be told apart from the agent's own variance, and a regression
flagged against `--baseline` is as likely to be noise as it is to be the skill.

Three is the smallest count that answers the question. One reports no spread,
two cannot say which of the pair was the outlier, and three shows that a cell is
unstable at all. `benchmark.json` reports mean, minimum, maximum and standard
deviation across the draws of each cell, and `iteration-summary` names the
assertions that flipped between them.

The same small count is what built the deltas in `benchmark.json` and
`iteration-summary.json`, and a bare subtraction does not say so. Every
`ScenarioDelta` (in `deltas` and in `iteration_comparison`) carries `left.runs`
and `right.runs`, the completed run count behind each side, plus a duration
median and MAD once an arm's draws clear the count `Dispersion` needs to
describe its own spread. `helped_by_skill` carries `with_skill_attempts` and
`without_skill_attempts` alongside its `delta` for the same reason: a pass rate
is a ratio, and the ratio alone hides whether it was drawn from three attempts
or thirty.

A `ScenarioDelta` also carries `assertion_pass_rate_interval` and
`run_pass_rate_interval`: the 95% interval the same draws leave around each
subtraction, as `{"low": …, "high": …}`. An interval that contains zero means
the arms have not been told apart, however large the number above it looks, and
three draws an arm is small enough that this is the ordinary case rather than
the exception. Both are computed by Newcombe's hybrid score method, built from
each arm's own Wilson interval rather than a pooled normal approximation,
because an arm that passed everything or failed everything is an ordinary
result here and is exactly where the normal approximation reports a width of
zero. Either field is absent when its side scored nothing, since a difference
against no draws has no width to report rather than an infinite one.

`--attempts 1` asks for a single draw, which is the right choice while writing a
case and reading its transcript. `--attempts 0` is refused, because a cell
nobody draws is a row the report cannot fill in.

This is the flag that decides the size of a pass, so it is the one to pair with
[`-j`](#running-more-than-one-run-at-a-time): three draws of two arms is six
times the work of a single arm drawn once, and the lanes are what keep that
inside a wall clock an operator will wait through. The cost is not reduced by
either flag. Each draw is also cached and reused under its own attempt number,
so re-running the same command does not pay for the draws again. See
[Reusing a completed run](#reusing-a-completed-run).

`--retries` is not a substitute. It re-invokes the harness only when a run
failed in transit, meaning a non-zero exit with no result or a timeout, and
never because an assertion failed, so it adds no draws to the distribution.

---

## Bounding what a pass may spend

`--attempts` defaulting to three draws means an ordinary pass now pays for
three model calls where it once paid for one, and `-j` lets more of them run at
once. Neither flag bounds what that costs, so a case count, a scenario count,
or a draw count entered wrong spends the difference before a report exists to
say so. `--max-cost-usd` is that bound.

The ledger it checks against is read before a run starts, not reserved for it,
so a pass can still spend past the ceiling by whatever the runs already in
flight cost when the check last passed. That is the price of a check cheap
enough to run before every one of them rather than one that has to coordinate
every lane in flight to answer.

Every invocation is charged to it, including the attempts `--retries` threw
away. The report keeps only the attempt that stuck, so a ceiling that counted
what the report shows would let a flaky pass bill several times over what it
was allowed.

Charged, that is, whenever the invocation came back with a price. An attempt
that timed out or died in the runner never reaches the event that carries one,
so there is no figure to charge and the ledger adds nothing: a missing price is
not a free run, it is one nobody can bill yet. A pass flaky enough to burn most
of its attempts that way can therefore spend past its ceiling without the
ledger seeing it. Nothing here can close that, since the only harness that
publishes a price publishes it once, at the end of a run that finished.

A run the ledger refuses is recorded with `status: skipped` and
`failure_kind: budget`, carrying a warning naming what the pass had spent and
the ceiling it hit. It is not a failed run: nothing was asked of the runner and
nothing about the skill was measured, so `grade` passes over it rather than
reading its empty workspace as a wrong answer, and it does not count against a
pass rate. A run already served from cache is unaffected
by the ceiling, since a cache hit costs nothing and reusing it is exactly what
a budget is for.

Only the `claude-code` runner publishes what a run cost today. A ceiling over
`codex` or `cursor-agent` would sit at zero spend for the life of the pass,
admit every run, and leave the operator believing a limit was holding while the
bill grew, so `--max-cost-usd` on either is refused at the command line before
a report exists to be read as bounded. Those passes still run without a
ceiling, and their `budget.spent` says `unpriced` and names the harness rather
than reporting a total of zero that nobody measured.

`trg` exits `2` instead of `0` when the pass got less than it asked for or paid
more than it allowed: either a run was refused, or spend went strictly past the
ceiling, which one run can do on its own because admission is checked rather
than reserved. A pass that lands exactly on the ceiling having refused nothing
is neither, and exits `0`: it did every run and paid what it said it would.
A genuine assertion or grading failure still exits `1`, which wins over `2`, so
a budget stop is not allowed to hide a suite that also failed on its merits. See [Exit codes](#exit-codes) and the `budget` field of
[`report.json`](#artifact-reportjson).

---

## Running more than one run at a time

A pass executes one run per case, per `--scenario` and per `--attempts` draw,
and each of those waits on a live agent. Serially, the wall clock is the product
of all four, which is how a suite small enough to read in a minute takes an hour
to execute. `-j N` keeps N runs in flight instead.

It cuts wall clock, not cost. Every run still pays for its own model calls, and
the lanes draw on the same account, so N is capped at 8: past that the requests
queue behind the provider's rate limit rather than the local CPU, and a run that
waits long enough there is indistinguishable from a slow one.

Each lane takes the next run nobody has claimed rather than a fixed share of
them, because runs are not equally long: one case can spend minutes with the
agent while the next is served from cache. Runs keep their suite order in
`report.json` whatever order the lanes finish in, so the same command compared
across two passes lines its runs up by position.

One thing changes meaning with more than one lane. `skill_integrity` reports
whether the skill directory a run was handed still holds what it held before,
and every lane reads the same directory, so a run that rewrites the skill
rewrites it for every lane still reading it. With `-j 1` the check is hashed
around each run and names that run. With `-j N` it is hashed once around the
whole pass, the finding is recorded on every run that executed, and each of
those runs carries a warning saying the change cannot be charged to it alone.

Either window can also fail to read the directory back, which is what a run that
deleted the skill leaves behind. That is reported as `tampered: true` with no
`tampered_files` and a warning naming the read failure, because a hash that
never came back is not a comparison that passed.

---

## Timeouts

When a run exceeds its timeout it is recorded with `status: timeout` and a
`duration_ms` equal to the limit, not to the time actually spent, because what
was spent past the limit is not a property of the skill.

The timeout stops the whole process tree, not just the harness. A harness spawns
tools and those tools spawn children, and every one of them is put in a process
group led by the harness so the group can be stopped as a unit: first a chance to
exit cleanly, so a harness can finish flushing the transcript the timed-out run
will be diagnosed from, then outright. A tool left running would keep consuming
provider quota for a run that is already over, keep writing into a workspace that
is about to be scored, and hold open the pipes the run's output is read through,
which is enough to make the timeout itself never return.

Because a harness leads its own process group, interrupting the terminal no
longer reaches it directly. `trg` stops every running harness on `SIGINT`,
`SIGTERM`, and `SIGHUP` before exiting, so an interrupted invocation does not
leave agents behind. A signal the invocation was started with ignored, as `nohup`
and most job runners arrange, is left ignored: an invocation set up to survive a
hangup keeps surviving it.

That group is a background one as far as the terminal is concerned, so each run
is given a closed stdin. A run receives its prompt on the command line, and a
background process that reads the controlling terminal is stopped rather than
answered, which would hang the run instead of ending it. A harness that reads
its stdin sees end of file immediately.

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

The shape below is generated from `NormalizedTranscript` and published as
`crates/trg/schemas/events.json.schema.json`, alongside the schemas for the
other artifacts on this page. The field table exists to walk through it in
prose; the schema is what `eval verify --mode strict` actually holds a bundle's
`events.json` to.

```json
{
  "runner": "claude",
  "tool_visibility": "observed",
  "events": [
    { "kind": "tool_call", "tool": "Read", "paths": [".skill/SKILL.md"] },
    { "kind": "assistant_text", "text": "..." },
    { "kind": "terminal", "ok": true }
  ],
  "workspace_escapes": [
    { "tool": "read", "path": "/somewhere/outside/notes.md" }
  ],
  "staged_skill": {
    "kind": "at",
    "directory": ".skill/",
    "name": { "kind": "known", "name": "demo-skill" }
  }
}
```

| Field | Type | Notes |
| ----- | ---- | ----- |
| `runner` | string | The program that produced the transcript |
| `tool_visibility` | enum | `observed` or `unavailable` |
| `events[].kind` | enum | `assistant_text`, `tool_call`, or `terminal` |
| `workspace_escapes[]` | array | Paths the run named that resolve outside its workspace; omitted when empty |
| `staged_skill.kind` | enum | `at` when the run staged a skill, `nothing` for the without-skill arm; omitted by a transcript written before trg recorded it |
| `staged_skill.directory` | string | The workspace-relative directory the skill was staged at, present with `at` |
| `staged_skill.name.kind` | enum | `known` when the run recorded the staged skill's name, `unrecorded` for a transcript written before trg recorded it; omitted when `unrecorded` |
| `staged_skill.name.name` | string | The name the staged skill answers to, present with `known` |

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
interpreter in `/bin/zsh -lc '...'` is not. Quoting says where an operand ends, so
a quoted path is one path however many spaces it holds, and the payload of an
interpreter flag is read as the command line it is. A search pattern can mention a path
without naming one, so it is left unchecked. A path beginning with `~` names the
host home directory rather than a directory in the workspace, so it is resolved
against `HOME` before the check. Every escape is also recorded as a run warning
in `report.json`. Detection is the remedy available here; prevention is not.

`staged_skill` is what `skill_used` is read against. `.skill/`, `.old-skill/`,
and `skills/<skill-name>/` are where *some* run stages a skill, so reading a
named path against all of them credits a without-skill run that looked into
`skills/` with using a skill it was never handed, and that run is the control the
arm gap is measured from. A run records where it staged, so the without-skill arm
reports no engagement at all, not even for a harness's own skill tool invoked on a
skill of the harness's own, and a with-skill run answers only for its own
directory. What a transcript cannot say is where a command ran, so a relative path
reached after a `cd`, or a search pattern quoting the staged directory, still reads
as a path to it.

`staged_skill.name` is what a native skill tool call is read against. A harness
invokes its own installed skills through the same tool, so a call is engagement
only when it names the skill this run staged, and the announced directories carry
no name for it to be matched against. A transcript written before the name was
recorded cannot say which skill a call invoked, which is not evidence that the
wrong one was, so it keeps the older reading and credits any such call.

`events.json` is normalized from the redacted transcript, so it carries no
secret that `transcript.jsonl` had stripped.
