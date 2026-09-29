---
name: trg-eval-hillclimb
description: Run one hillclimb round on a skill that already has a discriminating eval suite, change one thing, run both arms, and keep or revert by the held-out test split's verdict instead of by eye. Use when asked to improve, tune, or iterate on a SKILL.md that already clears the trg-eval-authoring bar, when a prior round's keep-or-revert recommendation needs to be acted on, or when someone is about to edit a skill with no plan for telling a real gain from noise.
---

# Hillclimb a trg skill

This skill assumes the bar `trg-eval-authoring` sets has already been cleared:
a suite exists, every case in it fails without the skill and passes with it,
and at least one case per claim is held out on `"split": "test"`. If that is
not true yet, stop and run `trg-eval-authoring` first; a hillclimb round
measured against a suite that cannot discriminate is not measuring anything.

## The rule the whole loop answers to

**Change exactly one thing per round. Keep it only when the test split's
verdict says `keep`.**

`eval iteration-summary --previous` reports `keep_or_revert.recommendation`:

| `recommendation` | What it means | What to do |
| ----------------- | -------------- | ---------- |
| `keep` | The held-out test split improved | Keep the change, start the next round |
| `revert` | The held-out test split regressed | Revert the change |
| `suspected_overfitting` | Train improved, test did not | Revert the change |
| `inconclusive` | Neither split moved far enough to say, or the suites being compared differ | Revert the change |

Only `keep` leaves a change in place. Everything else, including
`inconclusive`, reverts: a round with no positive evidence is not a round
worth keeping just because it was not a proven loss either.

## Step 1: check that the graders judging this are trustworthy

Skip this only if every grader in the suite is `mechanical`, `declarative`,
or `script`, none of which are ever gated this way. If any case uses an
`llm` grader, label a sample of its verdicts by hand and check agreement
before trusting a single round's recommendation to it:

```shell
trg ai skills eval feedback label ./artifacts/my-skill/<report-id> \
  --run run-001 --assertion case-a:g0 --verdict fail \
  --rationale 'evidence quoted the wrong file'

trg ai skills eval grader-agreement ./artifacts/my-skill/<report-id> --min-agreement 0.9
```

`--min-agreement` checks the `llm` bucket's Wilson 95% lower bound
specifically, and fails rather than passing silently when no `llm`-graded
assertion has a human label yet. Relabel or rewrite any grader that falls
below the threshold before leaning on its verdict in a hillclimb round. See
[Validate graders against human
labels](../../docs/how-to/validate-graders-against-human-labels.md).

## Step 2: change exactly one thing

Make one change to the skill under test: one paragraph reworded, one
instruction added, one example fixed. Note what the change is and why in the
round log (see below) before running anything. If the change cannot be
described in one sentence, it is more than one change, and a `revert` or
`keep` verdict on it will not say which part of it mattered.

## Step 3: run both arms in one pass

```shell
trg ai skills eval run --skill-dir ./skills/my-skill --out-dir ./artifacts \
  --runner claude-code --attempts 3 --grade --benchmark
```

Leave `--split` at its default of `all` and `--iteration` unset. A round
covers the whole suite in a single report bundle: `--iteration` auto-detects
the next number for that skill's `--out-dir`, and both `--previous` on later
commands and `--from` on `next-iteration` auto-detect the sibling report
bundle one iteration back by matching that number. Running the train and
test splits as two separate invocations would give both bundles the same
iteration number and break that auto-detection for every round after.

"Train split for the working signal, test split for the verdict" is a rule
about what you are allowed to *read* from this one bundle, covered next, not
a reason to run it twice.

## Step 4: read the train split, and only the train split, while you are still deciding

```shell
trg ai skills eval iteration-summary ./artifacts/my-skill/<report-id> \
  --withhold-test-detail
```

`by_split.train` and its transcripts are fair game: read them, read
`grading.json`, read a failing run's workspace, to understand whether the
change did anything at all before you spend a verdict on it.

**Do not open a test-split run's transcript, workspace, or grading detail at
any point in this loop.** The held-out split's job is to catch a change that
learned the suite instead of the task, and it can only do that if nothing
about a round's decisions was shaped by looking at it. `--withhold-test-detail`
is what makes that a rule the tool enforces rather than one you have to
remember: without it, a held-out case id or assertion shows up in several
places in the full document (the top-level stability lists, `by_split.test`,
its headroom warning) even though you only meant to read the train split. If
a number from `by_split.test` is not enough, that itself is information (see
saturation, next), not a reason to drop the flag and look closer.

## Step 5: check for saturation before reading the verdict as ordinary

`benchmark.json` and `iteration-summary.json` both report a `headroom`
field, and again per split under `by_split.train.headroom` and
`by_split.test.headroom`, whenever that split's `with_skill` arm has cleared
`--headroom-threshold` (a Wilson 95% lower bound, default `0.9`). Once the
**test** split has cleared it, no change can ever show up as `improved`
there again, no matter how good it is: there is no room left above the
floor for a better skill to reach.

`keep_or_revert.capped_by_saturation` is set to `test_split_saturated` in
exactly this case, so it never gets misread as an ordinary `inconclusive` or
a clean `keep`. When you see it: **stop hillclimbing this skill and fix the
suite instead.** Harden or add held-out cases until the test split has room
to move again; see [Assign each case to train or
test](../../docs/how-to/author-an-eval-suite.md#5-assign-each-case-to-train-or-test).
Continuing to tune the skill against a saturated test split only produces
`keep` recommendations that the suite was never able to earn.

## Step 6: get the verdict

```shell
trg ai skills eval iteration-summary ./artifacts/my-skill/<report-id> \
  --withhold-test-detail
```

Once there is a previous round to compare against, `--previous` auto-detects
it and the output carries a `keep_or_revert` section:

```json
{
  "suites": { "status": "same_suite" },
  "by_split": {
    "train": {
      "status": "improved",
      "assertion_pass_rate_delta": 0.12,
      "assertion_pass_rate_delta_interval": { "low": 0.03, "high": 0.21 }
    },
    "test": {
      "status": "indistinguishable",
      "assertion_pass_rate_delta": 0.02,
      "assertion_pass_rate_delta_interval": { "low": -0.08, "high": 0.12 }
    }
  },
  "recommendation": "suspected_overfitting"
}
```

`suites` reads `different_suites` instead of comparing when the eval suite
itself changed between the two rounds; `by_split` is then empty and
`recommendation` is `inconclusive`, since a pass-rate change against a
suite that gained or lost cases is not a change in the skill. Add
`--fail-on revert,overfitting` to turn the recommendation into an exit code
in a script or CI job driving this loop, rather than a line someone has to
notice. See [Reference: keep-or-revert
verdict](../../docs/reference/ai-skills-eval.md#keep-or-revert-verdict).

## Step 7: act on the recommendation

- **`keep`**: leave the change in place. Record the round as kept in the
  round log and go back to step 2 for the next round.
- **`revert`, `suspected_overfitting`, or `inconclusive`**: put the skill's
  files back to the state they were in before step 2's change. `trg` has no
  subcommand that does this for you; it is ordinary file or version-control
  management on the skill directory. Record the round as reverted, with the
  recommendation that caused it, and go back to step 2.

Either way, build the next round's improvement bundle from this round's
report before starting the next one:

```shell
trg ai skills eval next-iteration --from ./artifacts/my-skill/<report-id>
```

Read `improvement.md` and `improvement.json` for the next candidate change.
Their `held_out` field carries the test split's aggregate pass rate and run
counts, never its transcripts, prompts, or case ids, so reading them all the
way through never crosses the line drawn in step 4. See [Test-split cases
are withheld, not
shown](../../docs/reference/ai-skills-eval.md#test-split-cases-are-withheld-not-shown).

## Optional: catch ambiguous cases with `eval scaling`

A case that moves the wrong way when a plainly stronger configuration runs
it is often a sign of an ambiguous prompt or a grader that only happens to
agree with a weaker model's phrasing, rather than a real regression. Run a
weakest-to-strongest comparison across a few report bundles sharing the same
suite:

```shell
trg ai skills eval scaling ./report-weaker ./report-stronger
```

This is the same keep-or-revert comparison this loop runs, but across
configurations instead of across rounds of one skill. Cases listed under
`outliers` are worth a look before the next round, since they can point at a
suite problem this loop's own verdicts will not surface. See [Check model
scaling](../../docs/how-to/check-model-scaling.md).

## Keep a round log

Keep a plain log at `./artifacts/my-skill/hillclimb-log.md`, alongside the
report bundles rather than inside the skill directory. A file inside the
skill directory becomes part of what `--skill-staging copy` copies into
every run's workspace, and changes the skill's own content hash on every
round for a reason that has nothing to do with the change under test.

One entry per round is enough: the report id, the one-sentence change from
step 2, the `recommendation`, and whether it was kept or reverted. A round
capped by saturation is worth its own note, since it explains why a run
produced no usable verdict without anyone having to re-derive it from
`benchmark.json` later.

## What this skill will not do for you

- It will not pick the next change. That is step 2, informed by
  `improvement.md`'s aggregate numbers and your own reading of the train
  split, never by the test split's detail.
- It will not draw the train/test split or judge whether a case clears the
  eval-authoring bar. That is `trg-eval-authoring`'s job, done once before
  this loop starts.
- It will not revert a skill's files. There is no `eval` subcommand for
  that; it is file or version-control management outside `trg`.
- It will not relabel a grader for you. `eval feedback label` needs a human
  verdict behind every entry.

## Further reading

- [Author a trg eval suite](../trg-eval-authoring/SKILL.md)
- [Hillclimb a skill by hand](../../docs/how-to/hillclimb-a-skill.md)
- [Validate graders against human labels](../../docs/how-to/validate-graders-against-human-labels.md)
- [Check model scaling](../../docs/how-to/check-model-scaling.md)
- [Run an eval in CI](../../docs/how-to/run-in-ci.md)
- [Eval lifecycle](../../docs/explanation/eval-lifecycle.md)
- [AI skills eval reference](../../docs/reference/ai-skills-eval.md)
