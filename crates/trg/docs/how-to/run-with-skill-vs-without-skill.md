# Run with-skill vs without-skill evals

Compare agent output when your skill is loaded against a no-skill baseline in
one report bundle. Use this when you want to measure whether the skill improves
assertion pass rates, not just runner completion.

## Prerequisites

- A skill directory with `SKILL.md` and `evals/evals.json`
- Assertions defined for each eval case
- (Optional) An agent runner for live execution

## 1. Run both scenarios

Pass `--scenario` twice to include `with_skill` and `without_skill`:

```shell
$ trg ai skills eval run \
    --skill-dir ./skills/csv-analyzer \
    --out-dir ./artifacts \
    --runner cursor-agent \
    --scenario with_skill \
    --scenario without_skill
./artifacts/csv-analyzer/20260526T150000Z-aabbccdd
```

For two eval cases this produces four runs: each case once per scenario.

## 2. Grade completed runs

```shell
$ trg ai skills eval grade ./artifacts/csv-analyzer/20260526T150000Z-aabbccdd
./artifacts/csv-analyzer/20260526T150000Z-aabbccdd
Graded 4 run(s)
  assertions: 6/8 passed
```

## 3. Aggregate into benchmark.json

```shell
$ trg ai skills eval benchmark ./artifacts/csv-analyzer/20260526T150000Z-aabbccdd
./artifacts/csv-analyzer/20260526T150000Z-aabbccdd
```

## Generated artifacts

### report.json (trimmed)

```json
{
  "schema_version": "trg.skills-eval.report.v1",
  "report": {
    "id": "20260526T150000Z-aabbccdd",
    "iteration": 1
  },
  "suite": {
    "skill_name": "csv-analyzer",
    "skill_hash": "sha256:…",
    "evals_hash": "sha256:…"
  },
  "runs": [
    {
      "id": "run-001",
      "eval_case_id": "analyze-sales",
      "eval_slug": "analyze-sales",
      "scenario_id": "with_skill",
      "status": "completed",
      "metrics": { "duration_ms": 3200, "total_tokens": 4100, … }
    },
    {
      "id": "run-002",
      "eval_case_id": "analyze-sales",
      "scenario_id": "without_skill",
      "status": "completed",
      "metrics": { "duration_ms": 2800, "total_tokens": 2900, … }
    }
  ],
  "summaries": {
    "by_scenario": [
      { "scenario_id": "with_skill", "total_runs": 2, "passed_runs": 2, … },
      { "scenario_id": "without_skill", "total_runs": 2, "passed_runs": 1, … }
    ]
  }
}
```

### grading.json (per run, trimmed)

Written under `runs/run-001/grading.json` after `eval grade`:

```json
{
  "schema_version": "trg.skills-eval.grading.v1",
  "assertion_results": [
    {
      "assertion": "The workspace contains a summary file",
      "passed": true,
      "evidence": "Found summary.md (142 lines)",
      "grader": { "kind": "mechanical" }
    },
    {
      "assertion": "The summary mentions total revenue for May",
      "passed": true,
      "evidence": "Line 12 contains \"May revenue: 10000\"",
      "grader": { "kind": "mechanical" }
    }
  ],
  "summary": {
    "passed": 2,
    "failed": 0,
    "total": 2,
    "pass_rate": 1.0
  }
}
```

### benchmark.json (trimmed)

Written at `iteration-1/benchmark.json` (and report root) after `eval benchmark`:

```json
{
  "schema_version": "trg.skills-eval.benchmark.v1",
  "report_id": "20260526T150000Z-aabbccdd",
  "failed_runs_mode": "bucket",
  "scenarios": {
    "with_skill": {
      "completed": {
        "run_count": 2,
        "assertions": { "passed": 4, "failed": 0, "total": 4, "pass_rate": 1.0 },
        "duration_ms": { "mean": 3100.0, "p50": 3200, "p95": 3400, "total": 6200 },
        "tokens": { "total": 8200, … }
      }
    },
    "without_skill": {
      "completed": {
        "run_count": 2,
        "assertions": { "passed": 2, "failed": 2, "total": 4, "pass_rate": 0.5 },
        "duration_ms": { "mean": 2700.0, … },
        "tokens": { "total": 5800, … }
      }
    }
  },
  "deltas": {
    "with_skill_vs_without_skill": {
      "assertion_pass_rate": 0.5,
      "duration_ms_mean": 400.0,
      …
    }
  }
}
```

## What each scenario does

| Scenario | Skill available | Prompt |
| -------- | --------------- | ------ |
| `with_skill` | Symlinked to `.skill/` | Includes skill path and constraints |
| `without_skill` | Not present | Raw eval prompt only |

Both scenarios stage fixture files listed in each eval case's `files` array.
