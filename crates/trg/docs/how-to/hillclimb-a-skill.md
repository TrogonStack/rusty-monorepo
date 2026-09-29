# Hillclimb a skill

Take a skill whose eval suite already discriminates, holds out a `test`
split, and improve it one change at a time without fooling yourself about
whether a change helped. Use this once [Author an eval
suite](author-an-eval-suite.md) is done, not before: a round measured
against a suite that passes in both arms either way has nothing to report.

The agent-driven path is the `trg-eval-hillclimb` skill, shipped in this repo
at `crates/trg/skills/trg-eval-hillclimb/SKILL.md`. Give it to an agent and
it runs the loop below on your behalf, one round at a time. The rest of this
page is that same loop by hand.

## The rule

Change exactly one thing per round. Keep it only when the held-out test
split's verdict is `keep`; revert on `revert`, `suspected_overfitting`, or
`inconclusive`. Never read a test-split run's transcript, prompt, or grading
detail while deciding what to do next; only its aggregate pass rate, through
`keep_or_revert` or an improvement bundle's `held_out` field, is fair game.
Pass `--withhold-test-detail` to `iteration-summary` so that rule is enforced
by the command's own output rather than by what you choose not to look at.

## Prerequisites

- A skill directory whose eval suite passes [`eval verify --mode
  strict`](../reference/ai-skills-eval.md#eval-verify) and declares at least
  one `test`-split case
- An agent runner CLI (`claude-code`, `codex`, or `cursor-agent`)

## 1. Trust the graders before trusting a round

If any case uses an `llm` grader, label a sample of its verdicts and check
agreement before a round's recommendation rides on it:

```shell
$ trg ai skills eval feedback label ./artifacts/my-skill/<report-id> \
    --run run-001 --assertion case-a:g0 --verdict fail

$ trg ai skills eval grader-agreement ./artifacts/my-skill/<report-id> --min-agreement 0.9
```

`--min-agreement` checks the `llm` bucket's Wilson lower bound and fails
rather than passing silently when nothing has been labelled yet. See
[Validate graders against human
labels](validate-graders-against-human-labels.md).

## 2. Make one change

Edit `SKILL.md`. One change: one instruction, one example, one reworded
paragraph. Write down what it is before running anything; a change that
takes more than a sentence to describe is more than one change, and a
verdict on it will not say which part moved the number.

## 3. Run both arms in one pass

```shell
$ trg ai skills eval run \
    --skill-dir ./skills/my-skill \
    --out-dir ./artifacts \
    --runner claude-code \
    --attempts 3 \
    --grade \
    --benchmark
```

Leave `--split` at its default of `all` and `--iteration` unset, for the
whole suite in one report bundle. `--iteration` auto-detects the next number
for this skill's `--out-dir`, and every later `--previous` or `--from`
auto-detects the sibling bundle one number back. Splitting a round into a
`--split train` run and a `--split test` run would give both bundles the
same iteration number and break that auto-detection on the next round.

## 4. Read the train split while you decide

```shell
$ trg ai skills eval iteration-summary ./artifacts/my-skill/<report-id> \
    --withhold-test-detail
```

`by_split.train`, its transcripts, and `grading.json` are fair game: read
them to see whether the change did anything before spending a verdict on it.
`--withhold-test-detail` drops every held-out case id and assertion the
document would otherwise carry, in the top-level stability lists as well as
`by_split.test`, so there is nothing left to read there even by accident;
its aggregate counts and headroom warning still show whether it has data at
all.

## 5. Check for saturation

`benchmark.json` and `iteration-summary.json` report a `headroom` field, and
again per split under `by_split.train.headroom` / `by_split.test.headroom`,
once that split's `with_skill` arm has cleared `--headroom-threshold`
(default `0.9`). Once the **test** split has cleared it, no change can ever
register as `improved` there again. `keep_or_revert.capped_by_saturation`
is set to `test_split_saturated` in exactly this case. When it fires, stop
tuning the skill and harden the held-out cases instead; see [Assign each
case to train or test](author-an-eval-suite.md#5-assign-each-case-to-train-or-test)
and [Reference: headroom warning](../reference/ai-skills-eval.md#headroom-warning).

## 6. Get the verdict

```shell
$ trg ai skills eval iteration-summary ./artifacts/my-skill/<report-id> \
    --withhold-test-detail
```

Once a previous round exists, `--previous` auto-detects it and the output
carries `keep_or_revert.recommendation`:

| `recommendation` | Meaning | Action |
| ----------------- | ------- | ------ |
| `keep` | Test split improved | Keep the change |
| `revert` | Test split regressed | Revert the change |
| `suspected_overfitting` | Train improved, test did not | Revert the change |
| `inconclusive` | Neither split moved far enough, or the suites differ | Revert the change |

See [Reference: keep-or-revert
verdict](../reference/ai-skills-eval.md#keep-or-revert-verdict). Add
`--fail-on revert,overfitting` to turn this into an exit code in a script or
CI job; see [Keep-or-revert gate](run-in-ci.md#keep-or-revert-gate).

## 7. Act, then build the next bundle

On `keep`, leave the change in place. On anything else, put the skill's
files back to their state before step 2; `trg` has no subcommand for this,
it is file or version-control management. Either way, build the next
round's improvement bundle:

```shell
$ trg ai skills eval next-iteration --from ./artifacts/my-skill/<report-id>
```

Read `improvement.md` and `improvement.json` for the next candidate change.
Their `held_out` field carries the test split's aggregate pass rate and run
counts only, never its transcripts or case ids. See [Test-split cases are
withheld, not
shown](../reference/ai-skills-eval.md#test-split-cases-are-withheld-not-shown).
Then return to step 2.

## Optional: catch ambiguous cases

```shell
$ trg ai skills eval scaling ./report-weaker ./report-stronger
```

Compares the same keep-or-revert verdict across configurations instead of
across rounds. A case listed under `outliers` that got worse under a
plainly stronger configuration often points at an ambiguous prompt or a
grader tuned to a weaker model's phrasing. See [Check model
scaling](check-model-scaling.md).

## Keep a round log

Keep a plain log next to the report bundles, for example
`./artifacts/my-skill/hillclimb-log.md`, not inside the skill directory: a
file there becomes part of what `--skill-staging copy` stages into every
run and would change the skill's own content hash every round for reasons
unrelated to the change under test. One entry per round, naming the report
id, the change, the recommendation, and whether it was kept or reverted, is
enough to reconstruct the whole run later without re-deriving it from
`benchmark.json`.

## What this workflow does not automate

- Nothing picks the next change. That is step 2, informed by
  `improvement.md` and the train split, never the test split's detail.
- Nothing reverts a skill's files. That is version control, outside `trg`.
- Nothing relabels a grader. `eval feedback label` needs a human verdict
  behind every entry.
- Nothing stops you from tuning past a saturated test split. Step 5 is the
  only place that limit is enforced, by you.
