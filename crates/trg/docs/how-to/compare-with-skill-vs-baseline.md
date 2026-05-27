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

> **Status: planned** — `passed_runs` reflects runner completion status today,
> not assertion pass/fail. Once the grading PR populates `assertion_results`,
> summaries will track assertion outcomes.

## 3. Compare metrics manually

Timing data is available per run today:

```shell
$ jq -s '.' \
    ./artifacts/csv-analyzer/20260526T150000Z-aabbccdd/runs/run-001/timing.json \
    ./artifacts/csv-analyzer/20260526T150000Z-aabbccdd/runs/run-002/timing.json
[
  { "duration_ms": 3200, "total_tokens": 4100 },
  { "duration_ms": 2800, "total_tokens": 2900 }
]
```

## 4. Automated comparison (future)

> **Status: planned** — the `comparisons` array in `report.json` and standalone
> `comparison.json` files are scaffolded but empty. A future comparison PR will
> compute assertion deltas and metric deltas between scenarios automatically.

Until then, diff workspace outputs or grading files side by side:

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
