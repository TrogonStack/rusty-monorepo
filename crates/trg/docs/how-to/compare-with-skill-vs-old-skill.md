# Compare with-skill vs old-skill

Use this workflow when you have a prior skill revision and want to measure
regression or improvement against the current version.

## Current limitations

> **Status: planned** — `old_skill` scenario scaffolding exists in `eval run`,
> but agent runners reject it with `UnsupportedScenario` on `yordis/eval-2`.
> Staging a prior skill revision and invoking the runner for `old_skill` lands
> in a future PR.

You can still scaffold a three-scenario report today to validate layout and
reserve run slots:

```shell
$ trg ai skills eval run \
    --skill-dir ./skills/csv-analyzer \
    --out-dir ./artifacts \
    --scenario with_skill \
    --scenario without_skill \
    --scenario old_skill
./artifacts/csv-analyzer/20260526T160000Z-11223344
```

Without `--runner`, all runs are `skipped` — including `old_skill`:

```shell
$ jq '.runs[] | select(.scenario_id == "old_skill") | .status' \
    ./artifacts/csv-analyzer/20260526T160000Z-11223344/report.json
"skipped"
```

## Intended workflow (once old-skill runner lands)

### 1. Pin the old revision

Keep a copy of the previous skill tree (git tag, branch, or sibling directory):

```
skills/
├── csv-analyzer/          # current (--skill-dir)
└── csv-analyzer-v1/       # prior revision
```

### 2. Run three scenarios

```shell
$ trg ai skills eval run \
    --skill-dir ./skills/csv-analyzer \
    --out-dir ./artifacts \
    --runner cursor-agent \
    --scenario with_skill \
    --scenario without_skill \
    --scenario old_skill
```

Expected run matrix (2 eval cases × 3 scenarios = 6 runs):

| Run | Scenario | Skill loaded |
| --- | -------- | ------------ |
| 001 | `with_skill` | Current revision |
| 002 | `without_skill` | None |
| 003 | `old_skill` | Prior revision |
| 004 | `with_skill` | Current revision |
| … | … | … |

### 3. Compare outcomes

> **Status: planned** — automated `comparison.json` and populated
> `report.json` `comparisons` array will compute:

- Assertion pass-rate delta: `with_skill` vs `old_skill`
- Assertion pass-rate delta: `with_skill` vs `without_skill`
- Metric deltas (duration, tokens, cost)

Until then, verify each scenario's workspace independently:

```shell
$ trg ai skills eval verify ./artifacts/.../runs/run-001/workspace --mode strict
$ trg ai skills eval verify ./artifacts/.../runs/run-003/workspace --mode strict
```

## Why old-skill matters

The baseline (`without_skill`) tells you whether the agent can solve the task
at all. Comparing `with_skill` against `old_skill` tells you whether your
**changes** helped — isolating skill evolution from raw model capability.

## Workaround today

Run two separate reports — one against the current skill, one against the old
skill directory — and diff the outputs manually:

```shell
$ trg ai skills eval run \
    --skill-dir ./skills/csv-analyzer-v1 \
    --out-dir ./artifacts/v1 \
    --runner cursor-agent \
    --scenario with_skill

$ trg ai skills eval run \
    --skill-dir ./skills/csv-analyzer \
    --out-dir ./artifacts/v2 \
    --runner cursor-agent \
    --scenario with_skill
```

Compare `report.json` metrics and workspace contents across the two report
directories.

## Generated artifacts

Three-scenario reports include the same artifact shapes as a two-scenario run,
with an additional `old_skill` entry in `runs[]` and `summaries.by_scenario`.
See [Run with-skill vs without-skill](./run-with-skill-vs-without-skill.md#generated-artifacts)
for trimmed JSON examples.
