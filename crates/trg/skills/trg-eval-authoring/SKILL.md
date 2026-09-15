---
name: trg-eval-authoring
description: Author a trg eval suite for a skill that has none, or repair one that passes in both arms and therefore measures nothing. Use when asked to write, add, strengthen, or debug eval cases for a SKILL.md, when `trg ai skills eval init` has left its placeholder scaffold in place, or when a with-skill versus without-skill run reported a delta nobody can read.
---

# Author a trg eval suite

`trg ai skills eval init` writes a scaffold. The scaffold is a shape, not a
measurement. This skill takes you from that shape to a suite whose result means
something, using only commands `trg` already has.

## The bar every case must clear

**A case that passes in both arms measures nothing.**

Every eval runs twice: once with the skill staged (`with_skill`) and once
without it (`without_skill`). A case that passes in both says the agent could
already do the work; a case that fails in both says the case is broken or the
skill does not help. Only a case that **fails without the skill and passes with
it** is evidence the skill did anything.

Hold every case you write to that bar before you keep it. `trg` reports which
cases cleared it (step 7); it will not write them for you.

## Step 1: get the scaffold

```shell
$ trg ai skills eval init --skill-dir ./skills/my-skill
```

Writes `./skills/my-skill/evals/evals.json` with two placeholder cases,
`produces-a-summary` and `triggers-the-skill`. It refuses to overwrite an
existing manifest unless you pass `--force`. Use `--eval-dir` if the suite
belongs somewhere other than `evals/`.

Both placeholders are placeholders. Neither one is about your skill, so neither
one can clear the bar. Replace them.

## Step 2: turn the skill's claims into candidate cases

Read the target `SKILL.md` and list what it claims to change about an agent's
behaviour. Each claim is one candidate case. For each claim, answer both of
these before writing any JSON:

1. What does an agent produce for this request **without** the skill?
2. What observable difference does the skill's guidance make to that output?

If you cannot name a difference, there is no case here. Drop the claim or fix
the skill; do not write a case that will pass in both arms.

Then write the prompt as the request a user would actually make. Prompts under
20 characters and `expected_output` under 10 characters are flagged by the
linter as too vague.

## Step 3: state the difference as typed graders

Use a typed grader for anything mechanical. A typed grader is deterministic,
costs no model request, and cannot disagree with itself between runs. See
[Write graders](../../docs/how-to/write-graders.md) for when a judgment
genuinely needs the LLM judge instead.

The grader vocabulary:

| `type` | Settles |
| ------ | ------- |
| `file_exists` | whether a path under `outputs/` exists (or, with `"exists": false`, does not) |
| `contains` | whether a target holds a literal string |
| `regex` | whether a target matches a pattern, with `flags` (`i`, `m`, `s`), `count`, and `negate` |
| `valid_json` | whether a target parses as JSON |
| `schema_validation` | whether a target validates against a schema in the skill directory |
| `tool_used` | whether a tool was called, optionally narrowed by `input_match` |
| `tool_order` | whether two calls, or a sequence, happened in order |
| `skill_used` | whether the run engaged the skill |
| `llm` | a judgment no mechanical check answers |
| `baseline` | whether this run is at least as good as a reference output in the skill directory |

`target` selects what a grader reads: `final_text` (default), `transcript`,
`any_output`, `created_files`, `mock_calls`, `{"file": "..."}`, or
`{"files": "<glob>"}`.

A worked suite for a skill whose claim is "monthly totals, as a table, in
`summary.md`":

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
        { "type": "regex", "pattern": "^\\| May \\|", "target": { "file": "summary.md" }, "flags": "m" },
        { "type": "contains", "text": "TODO", "target": { "file": "summary.md" }, "negate": true }
      ]
    }
  ]
}
```

Each grader here names something an agent without the guidance plausibly gets
wrong: the file name, the figure, the table shape. A grader that any competent
agent satisfies by accident is a grader that will pass in both arms.

Fixture paths in `files` are relative to the skill directory and must exist when
the suite is checked.

## Step 4: write the triggering case with `skill_disclosure: unannounced`

Whether the agent **reaches for** the skill is a different question from whether
the skill helps once read. A prompt that names the skill and its directory has
already made the routing decision for the run. Set
`"skill_disclosure": "unannounced"` so the run makes it:

```json
{
  "id": "reaches-for-the-skill",
  "prompt": "I have a quarterly sales export in evals/files/sales.csv and I need the monthly revenue picture out of it.",
  "expected_output": "The run locates the skill on its own and follows it rather than improvising.",
  "files": ["evals/files/sales.csv"],
  "tags": ["triggering"],
  "skill_disclosure": "unannounced",
  "companion_skills": ["evals/companions/pdf-forms", "evals/companions/release-notes"],
  "graders": [
    { "type": "skill_used" }
  ]
}
```

`companion_skills` is the other half of the same question. A workspace holding
only the skill under test answers the routing decision for the run: there was
nothing else to reach for, so a skill that won reads the same as a skill that
was the only option. Each path names a skill directory of its own inside the
skill directory, with a `SKILL.md` whose `description` is plausible enough that
passing it over is a decision. They stage as siblings of the skill under test,
in every arm, and only an unannounced case may declare them.

Two things about `skill_used` that decide whether this case says anything:

- It is settled by whether the skill was staged, not by the run, so by default
  it is **reported in both arms and scored in neither**. It is not your delta;
  it is the routing check, read on its own.
- When the point of a case is that the skill must **not** be engaged, write
  `{"type": "skill_used", "negate": true, "arm": "both"}`, because failing it is
  then the finding.

The linter warns when a case checks skill engagement while the prompt announces
the skill, which is the mistake this step exists to prevent.

## Step 5: check the suite before spending a run

Runs cost money and wall clock. Everything that can be caught statically should
be caught here:

```shell
$ trg ai skills eval verify --skill-dir ./skills/my-skill --mode strict
```

`--mode strict` requires every case to declare at least one assertion or grader.
The same invocation prints the lint warnings: vague prompts, generic
`expected_output`, duplicate fixture paths, fixtures the prompt never mentions,
cases with no checks at all, and the announced-prompt mistake from step 4.

Fix every warning before running. `--lint-evals` on `eval run` prints the same
warnings, but only once you are already paying for the pass.

## Step 6: run both arms

```shell
$ trg ai skills eval run --skill-dir ./skills/my-skill --out-dir ./artifacts --runner claude-code --attempts 3 --grade --benchmark
```

`with_skill` and `without_skill` run together unless you name `--scenario`
yourself, so the plain command already produces the comparison. `--grade` scores
the runs and `--benchmark` aggregates them, which is what step 7 reads.

Notes that change what the number means:

- `--attempts 3` is the default and is what makes a score a measurement rather
  than one draw. Use `--attempts 1` only while writing a case and reading its
  transcript.
- Keep the default `--skill-staging copy`. Under `symlink` a run can follow the
  link to the eval suite sitting next to the skill and be scored on text it
  copied.
- Narrow with `--case <glob>` or `--tag <tag>` while iterating on one case.
- `-j` cuts wall clock, not cost. Every run still pays for its own model calls.

## Step 7: read the delta, and read it honestly

`benchmark.json` is written at the report root. Three places in it answer the
acceptance bar:

```json
{
  "iteration_summary": {
    "always_pass": ["totals-by-month: file 'summary.md' exists"],
    "helped_by_skill": ["totals-by-month: file 'summary.md' matches regex /^\\| May \\|/m"]
  },
  "deltas": {
    "with_skill_vs_without_skill": {
      "assertion_pass_rate": 0.5,
      "assertion_pass_rate_interval": { "low": -0.19, "high": 0.9 },
      "left": { "runs": 3 },
      "right": { "runs": 3 }
    }
  }
}
```

- `iteration_summary.helped_by_skill` names the checks that passed more often
  with the skill than without. **These are the only checks that cleared the
  bar.**
- `iteration_summary.always_pass` names checks that passed everywhere. Each one
  is a check that measured nothing.
- `deltas.with_skill_vs_without_skill.assertion_pass_rate` is the suite-level
  subtraction, and `assertion_pass_rate_interval` is the 95% interval around it.
  **An interval that contains zero means the arms have not been told apart**,
  however large the subtraction above it looks. `left.runs` and `right.runs` are
  what stood behind each side; at three draws an arm, an interval spanning zero
  is the ordinary result, not the exception.

Per-assertion detail, including how many attempts each rate was drawn from:

```shell
$ trg ai skills eval iteration-summary ./artifacts/my-skill/20260914T120000Z-abcd1234
```

Its `helped_by_skill` records carry `with_skill_pass_rate`,
`without_skill_pass_rate`, `delta`, `with_skill_attempts`, and
`without_skill_attempts`. A ratio without its attempt count hides whether it
came from three draws or thirty.

## Step 8: repair what measured nothing

For every check in `always_pass`, choose one:

- **The check is too easy.** Tighten it to something the guidance actually
  changes: the exact figure rather than any digit, the required section heading
  rather than any heading, the tool the skill tells the agent to use rather than
  any tool.
- **The claim is not real.** The skill does not change behaviour here. Remove
  the case, and consider whether the skill should still claim it.

For every check in `always_fail`, the case is usually broken rather than the
skill: a path the runner never writes, a regex that does not match its own
target, a fixture the prompt does not mention. Read one run's transcript and
`grading.json` before changing the skill.

Then go back to step 5. A suite is finished when every case you kept appears in
`helped_by_skill`.

## What trg will not do for you

Say so plainly rather than working around it:

- There is no interview, generator, or `--fix` that writes cases from a
  `SKILL.md`. Steps 2 through 4 are yours.
- There is no `eval check` subcommand. `eval verify --skill-dir` is the static
  check, and `--lint-evals` on `eval run` is the same lint mid-pass.
- Nothing rejects a suite for measuring nothing. Every case in it can pass in
  both arms and the pass still exits clean. Step 7 is the only place that bar is
  enforced, and you are the one enforcing it.

## Further reading

- [Write graders](../../docs/how-to/write-graders.md)
- [Author an eval suite by hand](../../docs/how-to/author-an-eval-suite.md)
- [Run with a skill vs without a skill](../../docs/how-to/run-with-skill-vs-without-skill.md)
- [AI skills eval reference](../../docs/reference/ai-skills-eval.md)
