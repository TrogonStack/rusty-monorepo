# Run skill evals in CI

Wire `trg ai skills eval` into a GitHub Actions (or similar) pipeline to catch
skill regressions on every pull request.

## What works today

| Step | Supported | Notes |
| ---- | --------- | ----- |
| Validate skill + eval suite | yes | `eval run` fails fast on invalid manifests |
| Scaffold report bundle | yes | Works without a runner (`status: skipped`) |
| Execute agent runs | yes | Requires runner CLI in PATH |
| Write `timing.json` | yes | When `--runner` is set |
| Grade assertions and graders | yes | `eval grade`, or `eval run --grade` |
| Verify `grading.json` | yes | `eval verify`, strict or lenient |
| Aggregate `benchmark.json` | yes | `eval benchmark`, or `eval run --benchmark` |
| Fail on assertion pass rate | yes | `--min-pass-rate`, or `--mode strict` on `verify` |
| Fail on regression against a baseline | yes | `--baseline` plus the `--fail-on-*` flags |
| Compare scenarios qualitatively | yes | `eval compare --judge llm` or `--judge script` |

## Minimal CI job (validation only)

No agent runner needed. This validates structure and writes the bundle:

```yaml
name: skill-eval
on:
  pull_request:
    paths:
      - 'skills/**'

jobs:
  validate:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install trg
        run: cargo install --path crates/trg

      - name: Scaffold eval bundle
        run: |
          trg ai skills eval run \
            --skill-dir ./skills/csv-analyzer \
            --out-dir ./artifacts \
            --scenario with_skill \
            --scenario without_skill

      - name: Upload artifacts
        uses: actions/upload-artifact@v4
        with:
          name: eval-report
          path: ./artifacts/
```

When running inside GitHub Actions, `report.json` automatically captures CI
context:

```json
{
  "ci": {
    "provider": "github-actions",
    "run_id": "12345678",
    "run_attempt": "1",
    "workflow": "skill-eval",
    "job": "validate",
    "commit": "abc123def456"
  }
}
```

## Full CI job (with runner)

Requires the agent CLI installed and authenticated on the runner:

```yaml
jobs:
  eval:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install trg
        run: cargo install --path crates/trg

      - name: Install cursor-agent
        run: npm install -g @cursor/agent-cli   # example; adjust for your runner

      - name: Run evals
        run: |
          REPORT=$(trg ai skills eval run \
            --skill-dir ./skills/csv-analyzer \
            --out-dir ./artifacts \
            --runner cursor-agent \
            --model-config ci-sonnet-4 \
            --scenario with_skill \
            --scenario without_skill)
          echo "REPORT_DIR=$REPORT" >> "$GITHUB_ENV"

      - name: Grade and verify
        run: |
          for ws in "$REPORT_DIR"/runs/*/workspace; do
            ./skills/csv-analyzer/evals/grade.sh "$ws" "$(basename "$(dirname "$ws")")"
            trg ai skills eval verify "$ws" --mode strict
          done

      - name: Upload report
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: eval-report
          path: ./artifacts/
```

## Strict vs lenient verification

| Mode | Missing `grading.json` | Failed assertions |
| ---- | ---------------------- | ----------------- |
| `lenient` (default) | allowed | reported, exit 0 |
| `strict` | error | error |

Use `--mode strict` once every run in the bundle is graded:

```shell
$ trg ai skills eval verify ./runs/run-001/workspace --mode strict
# exits 1 if any assertion failed or grading.json is missing
```

## Pass-rate thresholds

`eval verify` and `eval run` accept threshold flags directly, so the pipeline
does not need to post-process JSON:

```shell
$ trg ai skills eval verify ./runs/run-001/workspace \
    --min-pass-rate 0.9 \
    --max-tokens 500000 \
    --max-duration-ms 600000
```

| Flag | Fails when |
| ---- | ---------- |
| `--min-pass-rate RATE` | pass rate across the bundle is below `RATE` |
| `--max-tokens N` | total tokens across all runs exceed `N` |
| `--max-input-tokens N` | input tokens across all runs exceed `N` |
| `--max-output-tokens N` | output tokens across all runs exceed `N` |
| `--max-duration-ms MS` | any single run takes longer than `MS` |
| `--baseline REPORT_DIR` | regression flags below have something to compare against |

Regression gates compare the current bundle to a baseline report directory:

| Flag | Fails when |
| ---- | ---------- |
| `--fail-on-runner-failure` | any runner invocation failed |
| `--fail-on-failed-assertions` | any assertion result failed |
| `--fail-on-missing-grading` | a completed run workspace has no `grading.json` |
| `--fail-on-pass-rate-regression` | pass rate dropped below the baseline |
| `--fail-on-token-regression` | total tokens exceed the baseline |
| `--fail-on-duration-regression` | max duration exceeds the baseline |
| `--strict-ci` | shorthand that turns on every flag in this table |

The pass rate excludes `unsupported` results, so a runner that cannot be
observed does not drag the rate below the threshold. See
[Graders](../reference/ai-skills-eval.md#graders).

If you do want the raw number, it lives under `check.metrics` in the JSON
output:

```yaml
- name: Read pass rate
  run: |
    trg ai skills eval verify "$WS" --output-format json \
      | jq '.check.metrics.pass_rate'
```

## Benchmark aggregation

`benchmark.json` carries cross-run latency percentiles and token totals. Emit
it with `eval run --benchmark`, or aggregate an existing bundle afterwards:

```shell
$ trg ai skills eval benchmark ./artifacts/csv-analyzer/2026-01-15/report-001
```

## Tips

- Cache `target/` between CI runs when building `trg` from source.
- Use `--force` only in ephemeral CI workspaces (each run gets a fresh report ID).
- Upload artifacts on failure so reviewers can inspect workspaces.
- Pin `--model-config` to a stable label for trend comparison across commits.
- Run scaffold-only validation on every PR; reserve runner invocations for
  nightly or label-gated jobs to control cost.
