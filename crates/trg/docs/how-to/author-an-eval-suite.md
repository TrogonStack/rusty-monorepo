# Author an eval suite

Take a skill that has `SKILL.md` and no evals, and end with a suite whose result
means something. Use this when `trg ai skills eval init` has left its
placeholder scaffold in place, or when an existing suite passes everywhere and
reports no usable delta.

The agent-driven path is the `trg-eval-authoring` skill, shipped in this repo at
`crates/trg/skills/trg-eval-authoring/SKILL.md`. Give it to an agent and it
walks the sequence below on your behalf. The rest of this page is that same
sequence by hand.

## The bar

A case that passes in both arms measures nothing. Every eval runs once with the
skill staged and once without it, and only a case that **fails without the skill
and passes with it** is evidence the skill did anything. Hold every case to that
before keeping it.

## Prerequisites

- A skill directory containing `SKILL.md`
- An agent runner CLI (`claude-code`, `codex`, or `cursor-agent`) for live runs

## 1. Scaffold

```shell
$ trg ai skills eval init --skill-dir ./skills/my-skill
Created ./skills/my-skill/evals/evals.json
```

The scaffold declares two placeholder cases and passes `eval verify --mode
strict` unmodified. Neither placeholder is about your skill, so neither can
clear the bar. They are there to be replaced.

## 2. Turn each claim in `SKILL.md` into a candidate case

For each thing the skill claims to change, answer two questions: what does an
agent produce for this request without the skill, and what observable difference
does the guidance make. If you cannot name a difference, there is no case here.

Write the prompt as the request a user would really make. The linter flags
prompts shorter than 20 characters and `expected_output` shorter than 10.

## 3. State the difference as typed graders

Use a mechanical grader for anything it can answer: the verdict is
deterministic and costs no model request. Reserve the `llm` grader for
judgments no mechanical check answers. See [Write graders](write-graders.md)
for the full treatment, the grader list, and the `target` values.

```json
{
  "skill_name": "csv-analyzer",
  "evals": [
    {
      "id": "totals-by-month",
      "prompt": "Read evals/files/sales.csv and write outputs/summary.md with a markdown table giving each month's revenue total.",
      "expected_output": "outputs/summary.md holds a markdown table with one row per month in the fixture.",
      "files": ["evals/files/sales.csv"],
      "graders": [
        { "type": "file_exists", "path": "summary.md" },
        { "type": "contains", "text": "10000", "target": { "file": "summary.md" } },
        { "type": "regex", "pattern": "^\\| May \\|", "target": { "file": "summary.md" }, "flags": "m" }
      ]
    }
  ]
}
```

Each grader names something an agent without the guidance plausibly gets wrong.
A grader satisfied by accident will pass in both arms.

## 4. Add the triggering case

Whether the agent reaches for the skill is a separate question from whether the
skill helps once read, and a prompt naming the skill answers it for the run.
Declare `"skill_disclosure": "unannounced"` so the run answers it:

```json
{
  "id": "reaches-for-the-skill",
  "prompt": "I have a quarterly sales export in evals/files/sales.csv and I need the monthly revenue picture out of it.",
  "expected_output": "The run locates the skill on its own and follows it rather than improvising.",
  "files": ["evals/files/sales.csv"],
  "tags": ["triggering"],
  "skill_disclosure": "unannounced",
  "graders": [
    { "type": "skill_used" }
  ]
}
```

`skill_used` is settled by whether the skill was staged, so it is reported in
both arms and scored in neither. Read it on its own; it is not the delta.

## 5. Check the suite before spending a run

```shell
$ trg ai skills eval verify --skill-dir ./skills/my-skill --mode strict
```

`--mode strict` requires each case to declare at least one grader,
and the same invocation prints the lint warnings: vague prompts, generic
`expected_output`, duplicate fixture paths, fixtures the prompt never mentions,
and a case that checks skill engagement from a prompt that announces the skill.

`--lint-evals` on `eval run` prints the same warnings, but only once the pass is
already running.

## 6. Run both arms

```shell
$ trg ai skills eval run \
    --skill-dir ./skills/my-skill \
    --out-dir ./artifacts \
    --runner claude-code \
    --attempts 3 \
    --grade \
    --benchmark
```

`with_skill` and `without_skill` run together unless `--scenario` names
otherwise. Keep the default `--skill-staging copy`: under `symlink` a run can
follow the link to the eval suite next to the skill and be scored on text it
copied. See
[Run with a skill vs without a skill](run-with-skill-vs-without-skill.md) for
what each scenario stages.

## 7. Read the delta

`benchmark.json` answers the bar in three places:

- `iteration_summary.helped_by_skill` names the checks that passed more often
  with the skill than without. These are the only checks that cleared it.
- `iteration_summary.always_pass` names checks that passed everywhere. Each one
  measured nothing.
- `deltas.with_skill_vs_without_skill.assertion_pass_rate` is the suite-level
  subtraction, and `assertion_pass_rate_interval` is the 95% interval around it.
  An interval containing zero means the arms have not been told apart, however
  large the subtraction looks. At three draws an arm that is the ordinary
  result.

For per-assertion rates with the attempt counts behind them:

```shell
$ trg ai skills eval iteration-summary ./artifacts/my-skill/20260914T120000Z-abcd1234
```

## 8. Repair what measured nothing

For each check in `always_pass`, either tighten it to something the guidance
actually changes, or drop the case because the claim is not real. For each check
in `always_fail`, suspect the case before the skill: read one run's transcript
and `grading.json` first.

Then return to step 5. The suite is finished when every case you kept appears in
`helped_by_skill`.

## What this workflow does not automate

- Nothing generates cases from a `SKILL.md`. Steps 2 through 4 are authored.
- There is no `eval check` subcommand. `eval verify --skill-dir` is the static
  check.
- No gate rejects a suite for measuring nothing. A suite where every case passes
  in both arms still exits clean. Step 7 is where that bar is enforced, by you.
