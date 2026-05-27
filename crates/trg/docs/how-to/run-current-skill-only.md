# Run evals with the current skill only

Use this when you want to measure how the agent performs **with** your skill
loaded, without spending tokens on baseline or old-skill scenarios.

## Prerequisites

- A valid skill directory with `SKILL.md` and `evals/evals.json`
- (Optional) An agent runner installed (`cursor-agent`, `claude`, or `codex`)

## 1. Scaffold the report bundle

The default scenario is `with_skill`, so you only need `--skill-dir` and
`--out-dir`:

```shell
$ trg ai skills eval run \
    --skill-dir ./skills/csv-analyzer \
    --out-dir ./artifacts
./artifacts/csv-analyzer/20260526T143022Z-7f3a2b1c
```

Without `--runner`, every run is scaffolded with `status: skipped`. The command
still validates the skill, writes `report.json`, and creates empty workspace
directories — useful for CI layout checks.

Inspect the bundle:

```shell
$ ls ./artifacts/csv-analyzer/20260526T143022Z-7f3a2b1c
report.json  runs/

$ jq '.runs[] | {id, eval_case_id, scenario_id, status}' \
    ./artifacts/csv-analyzer/20260526T143022Z-7f3a2b1c/report.json
{
  "id": "run-001",
  "eval_case_id": "analyze-sales",
  "scenario_id": "with_skill",
  "status": "skipped"
}
```

## 2. Execute with an agent runner

Pass `--runner` to invoke the agent for each eval case:

```shell
$ trg ai skills eval run \
    --skill-dir ./skills/csv-analyzer \
    --out-dir ./artifacts \
    --runner cursor-agent \
    --runner-model sonnet-4 \
    --force
./artifacts/csv-analyzer/20260526T144501Z-c8d9e0f1
```

After completion, each run directory contains:

```
runs/run-001/
├── workspace/          # agent output and staged fixtures
├── transcript.jsonl    # raw stream-json from the runner
└── timing.json         # duration and token counts
```

Check run status in the report:

```shell
$ jq '.runs[0] | {status, metrics, skill_integrity}' \
    ./artifacts/csv-analyzer/20260526T144501Z-c8d9e0f1/report.json
{
  "status": "completed",
  "metrics": {
    "duration_ms": 4521,
    "total_tokens": 3840,
    "input_tokens": 2100,
    "output_tokens": 1740,
    "cost_usd": null
  },
  "skill_integrity": {
    "tampered": false,
    "tampered_files": []
  }
}
```

## 3. Verify outputs (optional)

> **Status: planned** — automatic `grading.json` emission is not yet wired.
> Until the grading PR lands, `verify` only validates files you write yourself.

When grading artifacts exist under the workspace:

```shell
$ trg ai skills eval verify \
    ./artifacts/csv-analyzer/20260526T144501Z-c8d9e0f1/runs/run-001/workspace
Bundle verified
  grading files: 1
  timing files: 0
  assertion results: 2/2 passed (100.00%)
```

Use `--mode strict` in CI once graders are producing `grading.json`.

## Tips

- Explicitly pass `--scenario with_skill` if your shell aliases or scripts set
  other scenarios by default.
- Use `--model-config prod-sonnet-4` to label the model configuration in
  `report.json` for later comparison across runs.
- Re-run with `--force` to overwrite a report directory with the same ID.

## Generated artifacts

### report.json (trimmed)

After `eval run`, the report directory contains:

```json
{
  "schema_version": "trg.skills-eval.report.v1",
  "report": {
    "id": "20260526T143022Z-7f3a2b1c",
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
      "scenario_id": "with_skill",
      "status": "skipped",
      "paths": { "workspace": "runs/run-001/workspace", … }
    }
  ],
  "dimensions": {
    "eval_cases": […],
    "assertions": […]
  }
}
```

After grading, each run may also contain `runs/run-001/grading.json` — see
[Write assertions](./write-assertions.md#generated-artifacts).
