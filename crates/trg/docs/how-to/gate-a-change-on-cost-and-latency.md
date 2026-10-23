# Gate a change on cost and latency

Use this workflow when a skill change has to keep its pass rate and also stay
inside a budget for cost, wall-clock time, or tokens per run. The keep-or-revert
recommendation reads pass rates only, so a change that holds its pass rate and
triples its cost is still a `keep`. The efficiency verdicts are the part of
`iteration-summary` that catches it.

## Prerequisites

- A baseline report bundle and a candidate report bundle of the same suite. Build
  them with `eval run --grade`, once before the change and once after it. See
  [Hillclimb a skill](hillclimb-a-skill.md).
- At least four completed `with_skill` runs per split in each bundle. Below that,
  a metric reads `unmeasured` instead of a verdict. The default of three attempts
  per case gives this with two cases or more.
- A `timing.json` for each run. For a cost verdict, every run on both sides must
  carry a price. A runner that prices nothing gives `unmeasured` with the reason
  `not_every_run_priced`.

## 1. Pick the increase you accept

Each metric takes a proportion of the baseline median per run. The default is
`0`, which fails any increase that the runs can tell apart from noise.

| Flag | Example | Reads as |
| ---- | ------- | -------- |
| `--max-cost-increase` | `0.1` | A run may cost up to 10% more than the baseline median |
| `--max-duration-increase` | `0.25` | A run may take up to 25% longer |
| `--max-token-increase` | `0` | A run may not spend more tokens in total |

A tolerance does not loosen the noise check. A metric regresses only when the
whole 95% interval of the per-run shift sits above the allowed increase. An
increase whose whole interval sits above zero and inside the allowance reads
`within_tolerance`. An increase whose interval sits above zero and reaches past
the allowance reads `increased`, because the runs cannot say whether it passes
the allowance. Neither one fails the job.

## 2. Compare the candidate with the baseline

```shell
$ trg ai skills eval iteration-summary ./artifacts/my-skill/candidate \
    --previous ./artifacts/my-skill/baseline \
    --max-duration-increase 0.5 \
    --max-token-increase 0.5 \
    --fail-on cost_regression,latency_regression,token_regression
```

The text output ends with one efficiency line for each split:

```text
Efficiency (allowed increase per run: cost 0, duration 0.5, tokens 0.5):
  train split: cost no runs; duration no runs; tokens no runs
  test split: cost costlier (+0.0900 USD/run, 95% CI [+0.0600, +0.1200], previous median 0.1450); duration within tolerance (+12000 ms/run, 95% CI [+9900, +14100], previous median 41750); tokens increased (+11000 tokens/run, 95% CI [+10100, +11900], previous median 20750)
iteration-summary: cost regressed on the test split, which --fail-on was told to fail on
```

Here the duration rose by about 12 seconds per run. The whole interval sits
above zero, so the increase is real. The high end, 14100 ms, is inside the 50%
allowance of 20875 ms, so it reads `within_tolerance` and does not regress.

The tokens rose by about 11000 per run. The low end, 10100, is inside the 50%
allowance of 10375 tokens, but the high end, 11900, is past it. The increase is
real, but the runs cannot say whether it passes the allowance, so it reads
`increased`. It does not regress, so `token_regression` does not match. Draw
more runs to narrow the interval.

The cost rose with no allowance, so `--fail-on cost_regression` exits `1`.

## 3. Gate the job

```yaml
- name: Gate on cost and latency against the baseline
  run: |
    trg ai skills eval iteration-summary "$CANDIDATE_REPORT_DIR" \
      --previous "$BASELINE_REPORT_DIR" \
      --max-cost-increase 0.1 \
      --max-duration-increase 0.25 \
      --fail-on revert,overfitting,cost_regression,latency_regression,token_regression
```

Exit `1` means at least one named condition matched, and stderr names each
one. Exit `0` with a `no_runs` or `unmeasured` metric means that the runs said
nothing about it. Read the verdicts in `iteration-summary.json` before you
treat a pass as a budget check.

## 4. Read the verdicts

The verdicts are in `keep_or_revert.efficiency.by_split` of
`iteration-summary.json`. Each metric carries a `status`. A measured metric
also carries the shift estimate, its interval, the baseline median and the run
counts behind each side.

| `status` | Meaning |
| -------- | ------- |
| `costlier`, `slower`, `more_tokens` | The whole interval sits above the allowed increase |
| `within_tolerance` | The whole interval sits above zero and inside the allowed increase. The increase is real, and the runs show that it stays inside the allowance |
| `increased` | The whole interval sits above zero, and it reaches past the allowed increase without sitting wholly above it. The increase is real, but the runs cannot say whether it passes the allowance |
| `cheaper`, `faster`, `fewer_tokens` | The whole interval sits below zero |
| `indistinguishable` | The interval reaches zero, so the runs cannot tell the sides apart |
| `unmeasured` | Too few runs reported the metric, or a run carried no price |
| `no_runs` | One side has no completed `with_skill` run on this split |

When the suites differ between the bundles, `by_split` is empty, and no
efficiency gate fires. See [Reference: efficiency
verdicts](../reference/ai-skills-eval.md#efficiency-verdicts) for the method.

## Related

- [Run skill evals in CI](run-in-ci.md#keep-or-revert-gate)
- [Hillclimb a skill](hillclimb-a-skill.md)
- [Reference: `eval iteration-summary`](../reference/ai-skills-eval.md#eval-iteration-summary)
