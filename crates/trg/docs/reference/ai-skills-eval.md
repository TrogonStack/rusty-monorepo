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
| `--environment` | enum | `scrubbed` | How much of the host machine each run may see; values: `scrubbed`, `isolated`, `inherited`. See [Run environment](#run-environment) |
| `--timeout-secs` | integer | *(unset)* | Per-run timeout. A case's `timeout_secs` overrides it. See [Timeouts](#timeouts) |
| `--case` | glob | *(unset)* | Cover only the cases whose `id` matches. Repeatable. See [Covering part of a suite](#covering-part-of-a-suite) |
| `--tag` | string | *(unset)* | Cover only the cases carrying this `tags` entry. Repeatable. See [Covering part of a suite](#covering-part-of-a-suite) |

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
| `skill_disclosure` | enum | no | `announced` (default) or `unannounced`. See [Measuring triggering](#measuring-triggering) |
| `tags` | string[] | no | Free-form labels. `--tag` selects by them. See [Covering part of a suite](#covering-part-of-a-suite) |
| `priority` | enum | no | `low`, `normal`, `high`, or `critical` |
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

Every grader also accepts `arm`, which decides whether its result counts toward
the score. See [Arm-scoped graders](#arm-scoped-graders).

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
failing it is the finding.

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
prompt's. See [Measuring triggering](#measuring-triggering).

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
| `environment` | string | Environment policy the runs were executed under: `scrubbed`, `isolated`, or `inherited` |
| `ci` | object | Present when running inside GitHub Actions (`GITHUB_ACTIONS=true`) |

### `suite` section

| Field | Type | Description |
| ----- | ---- | ----------- |
| `skill_name` | string | From skill frontmatter |
| `skill_path` | string | User-supplied `--skill-dir` path |
| `skill_hash` | string | `sha256:` digest of `SKILL.md` |
| `evals_path` | string | `<skill_path>/evals/evals.json` |
| `evals_hash` | string | `sha256:` digest of `evals.json` |
| `case_selection` | object | Present only when the run covered part of the suite. See [Covering part of a suite](#covering-part-of-a-suite) |

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

Schema version: `trg.skills-eval.grading.v4`. `v3`, `v2` and `v1` are still
accepted on read. `v2` added `unsupported` and narrowed `pass_rate` to scored
results only; `v3` makes `pass_rate` nullable, because a run where nothing could
be scored has no pass rate and reporting `0.0` reads as a total failure; `v4`
adds `excluded`, which takes an arm-scoped grader out of the score in both arms.

```json
{
  "schema_version": "trg.skills-eval.grading.v4",
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
      "excluded": "'skill_used' cannot hold without the skill, so it is reported in both arms and scored in neither; declare 'arm': 'both' to score it anyway"
    }
  ],
  "summary": {
    "passed": 1,
    "failed": 0,
    "unsupported": 1,
    "excluded": 1,
    "total": 3,
    "pass_rate": 1.0
  }
}
```

| Field | Type | Notes |
| ----- | ---- | ----- |
| `assertion_results[].assertion` | string | Non-empty. Accepts `text` as an alias. For a typed grader, its rendered description |
| `assertion_results[].passed` | bool | Pass/fail for this assertion. Always `false` when `unsupported` is present. An indicator rather than a score when `excluded` is present |
| `assertion_results[].evidence` | string | Non-empty. A passing result must not merely restate its assertion |
| `assertion_results[].grader.kind` | enum | `mechanical`, `declarative`, `llm`, `script`, `needs_llm`, or `none` |
| `assertion_results[].rationale` | string | Optional judge reasoning |
| `assertion_results[].unsupported` | string | Present when the runner cannot answer this check. Why it could not be graded |
| `assertion_results[].excluded` | string | Present when the grader presupposes the skill. Why it is reported rather than scored |
| `summary.passed` | integer | Must equal the count of scored, passing results |
| `summary.failed` | integer | Must equal the count of scored, failing results |
| `summary.unsupported` | integer | Must equal the count of results carrying `unsupported` and not `excluded` |
| `summary.excluded` | integer | Must equal the count of results carrying `excluded` |
| `summary.total` | integer | Must equal `assertion_results` length |
| `summary.pass_rate` | float or null | Must equal `passed / (total - unsupported - excluded)`, or `null` when nothing was scored |

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
when the run covered every case the suite declares.

`declared` names the whole suite the selection was taken from, since `dimensions.eval_cases`
lists only what the run covered. Suite drift is diffed against `declared`, so a case a run
skipped is not reported as one the suite lost, nor as one it gained the next time a run
covers it.

## Measuring triggering

A prompt that names the skill measures how well a run uses a skill it was
handed. It cannot also measure whether the skill's own `description` wins the
run's routing decision, because the prompt already made that decision. Those
are two different questions, and `skill_disclosure` says which one a case asks.

| `skill_disclosure` | Prompt | Skill staged at |
| ------------------ | ------ | --------------- |
| `announced` (default) | Names the staged directory, the skill `name`, and its `description` | `.skill/`, or `.old-skill/` in the `old_skill` arm |
| `unannounced` | Says nothing about the skill | `skills/<skill-name>/` |

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

---

## Scenario kinds

| Kind | CLI value | Runner behavior |
| ---- | --------- | --------------- |
| With skill | `with_skill` | Stages skill to `.skill/` in workspace; prompt prefixed with skill frontmatter. An unannounced case stages to `skills/<skill-name>/` and prefixes nothing |
| Without skill | `without_skill` | Raw eval prompt; nothing staged |
| Old skill | `old_skill` | Stages the `--old-skill-dir` revision to `.old-skill/` in the workspace; prompt prefixed with that revision's frontmatter |

### The eval suite is withheld from the workspace

The staged directory holds the skill under test minus its top-level `evals/`
directory. The suite is the answer key: it carries each case's
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

Every policy writes the variables a run actually received to `env.json` in the
run directory, with secret-looking values left out.

The policy is part of a run's cache identity, alongside `--skill-staging`. A
completed run answers only for what it was allowed to see, so switching either
flag executes again rather than serving a run that saw something else.

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
  ],
  "staged_skill": { "kind": "at", "directory": ".skill/" }
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

`events.json` is normalized from the redacted transcript, so it carries no
secret that `transcript.jsonl` had stripped.
