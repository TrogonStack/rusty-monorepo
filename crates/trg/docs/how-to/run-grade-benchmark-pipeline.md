# Run, grade, and benchmark in one command

Use `trg ai skills eval run` with `--grade` and `--benchmark` when you want the
full eval pipeline without shelling out to three separate commands. This is the
usual choice for CI jobs that execute agent runs, grade assertions, and emit
`benchmark.json` for dashboards or gates.

## One-shot invocation

```shell
trg ai skills eval run \
  --skill-dir ./skills/my-skill \
  --out-dir ./artifacts \
  --runner codex \
  --model-config ci-default \
  --scenario with_skill \
  --scenario without_skill \
  --grade \
  --benchmark
```

Pipeline order is fixed: **run → grade → benchmark**. If run fails, later
stages are skipped. If grade fails after a successful run, the report directory
is left on disk (including `report.json`) so you can inspect or re-run grading
manually.

## Flags and defaults

| Flag | Effect |
| ---- | ------ |
| `--grade` | Writes per-run `grading.json` and updates `report.json` (same defaults as `eval grade`) |
| `--benchmark` | Writes `benchmark.json` at the report root (same defaults as `eval benchmark`) |

`--benchmark` without `--grade` is allowed. The benchmark will record
`missing_grading` for completed runs that have no grading artifacts; it does not
crash.

Grader, strict, and `--failed-runs` options are **not** duplicated on `run`. Use
separate `eval grade` / `eval benchmark` commands when you need that control.

## JSON output

`--json` on `run` prints machine-readable output for the **final** stage only:

- `--benchmark` → benchmark document wrapper
- `--grade` only → grade summary wrapper
- neither → existing run CI summary (`EvalCommandJsonOutput`)

Intermediate stages are not emitted to stdout.

## When to use separate commands

Keep the composable workflow for local debugging:

```shell
REPORT=$(trg ai skills eval run --skill-dir ./skills/my-skill --out-dir ./artifacts --runner codex)
trg ai skills eval grade "$REPORT" --grader script --grader-command ./grade.sh --strict
trg ai skills eval benchmark "$REPORT" --failed-runs exclude
```

## Related docs

- [Run skill evals in CI](./run-in-ci.md)
- [Write assertions](./write-assertions.md)
- [Write mechanical grader scripts](./write-mechanical-grader-scripts.md)
