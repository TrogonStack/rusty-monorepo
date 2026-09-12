# Compare with-skill vs baseline (without-skill)

Measure whether your skill improves agent output by running the same eval cases
under both `with_skill` and `without_skill` scenarios in a single report.

## Prerequisites

- Eval suite with assertions defined in `evals/evals.json`
- Agent runner installed

## 1. Run both scenarios

Pass `--scenario` twice (order determines run numbering):

```shell
$ trg ai skills eval run \
    --skill-dir ./skills/csv-analyzer \
    --out-dir ./artifacts \
    --runner cursor-agent \
    --scenario with_skill \
    --scenario without_skill
./artifacts/csv-analyzer/20260526T150000Z-aabbccdd
```

For two eval cases this produces four runs:

| Run ID | Eval case | Scenario |
| ------ | --------- | -------- |
| `run-001` | first case | `with_skill` |
| `run-002` | first case | `without_skill` |
| `run-003` | second case | `with_skill` |
| `run-004` | second case | `without_skill` |

Confirm in the report:

```shell
$ jq '.runs[] | {id, eval_case_id, scenario_id, status}' \
    ./artifacts/csv-analyzer/20260526T150000Z-aabbccdd/report.json
{
  "id": "run-001",
  "eval_case_id": "analyze-sales",
  "scenario_id": "with_skill",
  "status": "completed"
}
{
  "id": "run-002",
  "eval_case_id": "analyze-sales",
  "scenario_id": "without_skill",
  "status": "completed"
}
...
```

## 2. Inspect per-scenario summaries

`report.json` includes aggregated counts:

```shell
$ jq '.summaries.by_scenario' \
    ./artifacts/csv-analyzer/20260526T150000Z-aabbccdd/report.json
[
  {
    "scenario_id": "with_skill",
    "total_runs": 2,
    "passed_runs": 2,
    "skipped_runs": 0,
    "failed_runs": 0
  },
  {
    "scenario_id": "without_skill",
    "total_runs": 2,
    "passed_runs": 1,
    "skipped_runs": 0,
    "failed_runs": 1
  }
]
```

`passed_runs` counts runs the runner completed, not assertions that passed. A
run that finished and then failed every assertion still counts as passed here.
For assertion outcomes, grade the bundle and read `benchmark.json`.

## 3. Grade both scenarios

```shell
$ trg ai skills eval grade ./artifacts/csv-analyzer/20260526T150000Z-aabbccdd
```

Or add `--grade` to the `eval run` above to do it in the same invocation. Each
run workspace then holds a `grading.json`:

```shell
$ jq '.summary' \
    ./artifacts/csv-analyzer/20260526T150000Z-aabbccdd/runs/run-001/workspace/grading.json
{
  "passed": 3,
  "failed": 1,
  "total": 4,
  "unsupported": 0,
  "pass_rate": 0.75
}
```

## 4. Read the scenario delta

`eval benchmark` aggregates the graded runs into per-scenario buckets and
computes the delta between them:

```shell
$ trg ai skills eval benchmark ./artifacts/csv-analyzer/20260526T150000Z-aabbccdd
$ jq '.deltas.with_skill_vs_without_skill' \
    ./artifacts/csv-analyzer/20260526T150000Z-aabbccdd/benchmark.json
{
  "assertion_pass_rate": 0.375,
  "run_pass_rate": 0.5,
  "duration_ms_mean": 400.0,
  "tokens_total": 1200
}
```

A positive `assertion_pass_rate` means the skill helped. `duration_ms_mean` and
`tokens_total` are the cost of that help. Per-scenario percentiles live under
`scenarios.with_skill.completed.duration_ms` (`mean`, `p50`, `p95`).

Run the whole chain in one invocation with
`eval run --grade --benchmark`.

## 5. Ask a judge which output is better

Pass-rate deltas do not capture output quality. `eval compare` puts the two
outputs in front of a judge under blind labels:

```shell
$ trg ai skills eval compare ./artifacts/csv-analyzer/20260526T150000Z-aabbccdd \
    --pair with_skill:without_skill \
    --judge llm \
    --judge-model gpt-4o
```

Records merge into the `comparisons` array in `report.json`. `--judge none` is
the default and emits nothing, so the flag is required for this step. See
[Compare with-skill vs old-skill](./compare-with-skill-vs-old-skill.md#3-compare-outcomes)
for the record shape and the script judge contract.

To eyeball the raw difference instead, diff the workspaces:

```shell
$ diff -ru \
    ./artifacts/.../runs/run-001/workspace \
    ./artifacts/.../runs/run-002/workspace
```

## What each scenario does

| Scenario | Skill available | Prompt |
| -------- | --------------- | ------ |
| `with_skill` | Symlinked to `.skill/` | Prefixed with full `SKILL.md` contents |
| `without_skill` | Not present | Raw eval prompt only |

Both scenarios stage fixture files listed in the eval case's `files` array.

## Generated artifacts

See [Run with-skill vs without-skill](./run-with-skill-vs-without-skill.md#generated-artifacts)
for trimmed `report.json`, `grading.json`, and `benchmark.json` examples from a
two-scenario run.
