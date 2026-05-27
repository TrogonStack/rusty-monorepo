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
| Verify `grading.json` | yes | When graders write the file |
| Fail on assertion pass rate | partial | `--mode strict` on `verify` |
| Auto-grade assertions | no | Planned grading PR |
| CI pass-rate thresholds | no | Planned CI thresholds PR |

## Minimal CI job (validation only)

No agent runner needed — validates structure and writes the bundle:

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

Use `--mode strict` once graders are wired:

```shell
$ trg ai skills eval verify ./runs/run-001/workspace --mode strict
# exits 1 if any assertion failed or grading.json is missing
```

## Pass-rate thresholds

> **Status: planned** — a future CI thresholds PR will add flags like
> `--min-pass-rate 0.9` to `eval verify` (or a dedicated `eval gate`
> subcommand) so pipelines fail when assertion pass rate drops below a
> configured minimum.

Until then, parse JSON output in your workflow:

```yaml
- name: Check pass rate
  run: |
    RATE=$(trg ai skills eval verify "$WS" --format json | jq '.pass_rate')
    python3 -c "import sys; sys.exit(0 if float('$RATE') >= 0.9 else 1)"
```

## Benchmark aggregation

> **Status: planned** — `benchmark.json` with cross-run p50/p95 latency and
> token totals is not emitted yet. Track `timing.json` per run manually or
> aggregate from `report.json` run metrics.

## Tips

- Cache `target/` between CI runs when building `trg` from source.
- Use `--force` only in ephemeral CI workspaces (each run gets a fresh report ID).
- Upload artifacts on failure so reviewers can inspect workspaces.
- Pin `--model-config` to a stable label for trend comparison across commits.
- Run scaffold-only validation on every PR; reserve runner invocations for
  nightly or label-gated jobs to control cost.
