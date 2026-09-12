# Compare with-skill vs old-skill

Use this workflow when you have a prior skill revision and want to measure
regression or improvement against the current version.

## 1. Pin the old revision

Keep a copy of the previous skill tree (git tag, branch, or sibling directory):

```text
skills/
├── csv-analyzer/          # current (--skill-dir)
└── csv-analyzer-v1/       # prior revision (--old-skill-dir)
```

The old revision must declare the same `name` as the current one. Pass
`--allow-skill-name-mismatch` if the rename is deliberate.

## 2. Run three scenarios

```shell
$ trg ai skills eval run \
    --skill-dir ./skills/csv-analyzer \
    --old-skill-dir ./skills/csv-analyzer-v1 \
    --out-dir ./artifacts \
    --runner cursor-agent \
    --scenario with_skill \
    --scenario without_skill \
    --scenario old_skill \
    --grade
```

Expected run matrix (2 eval cases × 3 scenarios = 6 runs):

| Run | Scenario | Skill loaded | Staged at |
| --- | -------- | ------------ | --------- |
| 001 | `with_skill` | Current revision | `.skill/` |
| 002 | `without_skill` | None | n/a |
| 003 | `old_skill` | Prior revision | `.old-skill/` |
| 004 | `with_skill` | Current revision | `.skill/` |
| … | … | … | … |

`--old-skill-dir` is required whenever `old_skill` is in the scenario list, and
`eval run` validates that directory as a skill before any runner starts.

## 3. Compare outcomes

`eval compare` asks a judge which arm produced the better answer for each eval
case, so it needs a judge. `--judge none` is the default and returns nothing:

```shell
$ trg ai skills eval compare ./artifacts/csv-analyzer/20260526T160000Z-11223344 \
    --pair with_skill:old_skill \
    --pair with_skill:without_skill \
    --judge llm \
    --judge-model gpt-4o \
    --emit-comparison-json
```

Each pair yields one record per eval case, with a `winner` of `A`, `B`, or
`tie`, the judge's `evidence`, the rubric it scored against (`organization`,
`formatting`, `completeness`, `usefulness`, `polish`, `domain_fit`), and which
backend answered. Records merge into the `comparisons` array in `report.json`;
`--emit-comparison-json` additionally writes `comparison.json` under the
iteration layout directories.

The two outputs are presented to the judge under blind labels `A` and `B`, with
the assignment derived from a hash of the eval case ID, so the judge cannot
learn which arm is the new skill from position alone. `mapping` in the record
un-blinds the labels after the fact.

Use `--judge script --judge-command ./judge.sh` to supply your own judge; it
reads `{eval_case_id, prompt, expected_output, rubric, outputs}` on stdin and
writes `{winner, evidence}` on stdout. See
[the judge configuration](../reference/ai-skills-eval.md#the-llm-judge) for LLM
credentials.

## 4. Read the numeric delta

Comparison records carry qualitative verdicts, not numbers. The scenario delta
lives in `benchmark.json` under `deltas.with_skill_vs_old_skill`:

```shell
$ trg ai skills eval benchmark ./artifacts/csv-analyzer/20260526T160000Z-11223344
$ jq '.deltas.with_skill_vs_old_skill' \
    ./artifacts/csv-analyzer/20260526T160000Z-11223344/benchmark.json
{
  "assertion_pass_rate": 0.25,
  "run_pass_rate": 0.5,
  "duration_ms_mean": -820.5,
  "tokens_total": -1420
}
```

A positive `assertion_pass_rate` means the current revision graded better than
the prior one.

## Why old-skill matters

The baseline (`without_skill`) tells you whether the agent can solve the task at
all. Comparing `with_skill` against `old_skill` tells you whether your
**changes** helped, which isolates skill evolution from raw model capability.

## Scaffolding without a runner

Omitting `--runner` still builds the three-scenario report so you can validate
layout before spending tokens. All runs land as `skipped`, including
`old_skill`:

```shell
$ jq '.runs[] | select(.scenario_id == "old_skill") | .status' \
    ./artifacts/csv-analyzer/20260526T160000Z-11223344/report.json
"skipped"
```

## Generated artifacts

Three-scenario reports include the same artifact shapes as a two-scenario run,
with an additional `old_skill` entry in `runs[]` and `summaries.by_scenario`.
See [Run with-skill vs without-skill](./run-with-skill-vs-without-skill.md#generated-artifacts)
for trimmed JSON examples.
