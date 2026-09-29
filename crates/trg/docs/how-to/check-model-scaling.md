# Check model scaling

Swapping a skill's runner model for a larger one, or a cheaper one, is a
claim: this configuration scores at least as well as that one. Eyeballing
two `benchmark.json` pass rates side by side answers nothing when the
suite is small enough that a couple of flipped assertions move the rate by
more than the gap you are trying to see. `eval scaling` runs the same
keep-or-revert comparison a hillclimbing iteration is judged by, but
across configurations instead of across iterations of one skill.

## Run each configuration and grade it

Produce one report bundle per configuration, against the exact same suite:

```shell
trg ai skills eval run --skill-dir ./my-skill --runner claude-code \
  --runner-model claude-haiku-4-5 --out-dir ./report-small --grade

trg ai skills eval run --skill-dir ./my-skill --runner claude-code \
  --runner-model claude-opus-5-5 --out-dir ./report-large --grade
```

Every bundle records its own captured model config in
`dimensions.model_configs`, including where the model name came from (the
eval case, a runner flag, or a runner default). `eval scaling` reads that
back and names each side by it, so a step's verdict is never read against
an unlabelled bundle.

## Compare them

```shell
trg ai skills eval scaling ./report-small ./report-large
```

List the bundles weakest to strongest. This command never orders them for
you and never infers which model is "bigger": the order you give is the
claim being checked. With three or more bundles, every adjacent pair is
compared, plus one end-to-end comparison of the first against the last, so
a middle step's regression is not hidden by a curve that happens to recover
by the final configuration.

Each comparison reads the `with_skill` assertion pass rate on both sides
and classifies the gap with a Newcombe interval around the difference:
`improved` when the interval sits entirely above zero, `regressed` when it
sits entirely below, `flat` when it straddles zero, and `no_runs` when
neither side has a `with_skill` run to compare at all. The same
classification runs again per split (`train`, `test`), since a suite whose
cases are split for hillclimbing can move differently on each.

`scaling.json` is written to the **last** directory you listed, the one you
named as strongest, alongside its own `report.json`.

## Read the outliers

A configuration can win on the overall rate while losing ground on a
specific case. Every case where the stronger side scored lower than the
weaker side is listed under that step's `outliers`, with both sides' pass
rate for that case:

```shell
$ trg ai skills eval scaling ./report-small ./report-large
Model scaling across 2 bundles:
  small-run -> large-run: improved (0.750 -> 0.917)
    train: improved (0.750 -> 0.917)
    test: no with_skill runs on either side
    outlier: refund-edge-case scored 1.000 -> 0.000
End to end:
  small-run -> large-run: improved (0.750 -> 0.917)
report_dir: ./report-large
```

An outlier is not itself a verdict on the pair: with a handful of draws a
single case moves on noise the same way an overall rate does. It is worth
an author's look regardless, since a case a larger model gets wrong is
often a sign of an ambiguous prompt or a grader that only happens to agree
with the smaller model's phrasing.

## Gate on it

```shell
trg ai skills eval scaling ./report-small ./report-large --fail-on-regression
```

`--fail-on-regression` fails the command when any step, on any split, or
the end-to-end comparison came back `regressed`. Put the bundle you are
proposing to switch to last: the gate reads the direction of the list you
give it, not any notion of which model is objectively stronger, so listing
a downgrade last and running with `--fail-on-regression` catches exactly
that.

## Related docs

- [`eval scaling` reference](../reference/ai-skills-eval.md#eval-scaling)
- [Artifact: `scaling.json`](../reference/ai-skills-eval.md#artifact-scalingjson)
- [Eval lifecycle](../explanation/eval-lifecycle.md)
