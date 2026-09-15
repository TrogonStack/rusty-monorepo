# Write eval graders

Typed `graders` are the way an eval case states what it checks. Reach for an
explicit `{"type": "llm", ...}` grader for the judgments a machine cannot make
on its own; use a mechanical grader for everything else.

## A worked example

```json
{
  "skill_name": "csv-analyzer",
  "evals": [
    {
      "id": "analyze-sales",
      "prompt": "Analyze evals/files/sales.csv and write a summary.",
      "expected_output": "A markdown summary with revenue totals by month.",
      "files": ["evals/files/sales.csv"],
      "graders": [
        { "type": "file_exists", "path": "summary.md" },
        { "type": "contains", "text": "May", "target": { "file": "summary.md" } },
        { "type": "regex", "pattern": "\\$[0-9,]+", "target": { "file": "summary.md" } },
        { "type": "tool_used", "tool": "Read" },
        {
          "type": "llm",
          "criterion": "The summary reads as a coherent narrative rather than a data dump.",
          "target": { "file": "summary.md" }
        }
      ]
    }
  ]
}
```

Four of these five checks are mechanical: whether `summary.md` exists, whether
it mentions May, whether it names a dollar figure, whether the agent read the
fixture. None of them can disagree with itself between runs, and none costs a
request. The fifth, whether the summary reads as a narrative rather than a
dump of numbers, is a judgment call, so it is written as an `llm` grader
instead of being forced into a mechanical pattern that could only approximate
it.

Every grader appears as a dimension entry in `report.json`, under
`dimensions.assertions`, with id `<eval-case-id>:g<index>` (zero-based). See
[Graders](../reference/ai-skills-eval.md#graders) for the full grader list and
the `target` values.

## Writing effective checks

| Do | Don't |
| -- | ----- |
| Describe observable outcomes in the workspace | Require exact wording |
| One check per grader | Bundle unrelated checks |
| Name acceptable file patterns (`summary.md` or `report.md`) | Hard-code a single filename the agent might not choose |
| Reference domain facts from fixtures ("May revenue") | Repeat the entire prompt |

### Example: strong vs weak

**Weak** (too vague, hard to grade consistently):

```json trg-example=skip
"graders": [{ "type": "llm", "criterion": "The output is good" }]
```

**Strong** (checks concrete artifacts):

```json trg-example=skip
"graders": [
  { "type": "regex", "pattern": "May.*10000|10000.*May" },
  { "type": "regex", "pattern": "(?i)error", "negate": true }
]
```

Both of these are mechanical once stated this precisely: the first is a
`regex` grader against the known revenue figure, and the second is a negated
`regex` for an error pattern. Writing them mechanically leaves the verdict to a
check that says what it looks at, rather than to a judge asked to read the
whole workspace.

## Pair fixtures with checks

Use the `files` array to stage inputs the agent needs:

```json
{
  "id": "merge-reports",
  "prompt": "Merge evals/files/q1.csv and evals/files/q2.csv into a single report.",
  "expected_output": "Combined quarterly report.",
  "files": [
    "evals/files/q1.csv",
    "evals/files/q2.csv"
  ],
  "graders": [
    { "type": "llm", "criterion": "The workspace contains exactly one combined output file." },
    { "type": "llm", "criterion": "The combined output includes rows from both input files." }
  ]
}
```

Neither check has a typed mechanical equivalent: no grader counts the files a
run produced, or reconciles two source files against one output, so both stay
with the judge. That is the deliberate case for `llm`, distinct from the
worked example above: not "this could be mechanical but isn't written that
way yet," but "trg has no mechanism that answers this."

Paths must be relative to the skill directory and must exist at validation
time.

## How grading consumes graders

`trg ai skills eval grade <report-dir>` (or `eval run --grade`) writes one
`grading.json` per run, with one entry per grader:

```json
{
  "assertion": "file 'summary.md' exists",
  "passed": true,
  "evidence": "'.../workspace/summary.md' exists and holds 142 bytes",
  "grader": { "kind": "declarative" }
}
```

`evidence` must be a concrete observation. Grading rejects evidence that merely
restates the check, because a passing result nobody can check is worse than no
result.

Verify a graded bundle with:

```shell
trg ai skills eval verify ./runs/run-001/workspace --mode strict
```

## Validation rules

- Graders are optional by default. Pass `--require-graders` (or verify with
  `--mode strict`) to require that each case declares at least one.
- Duplicate eval case IDs are rejected.
- `skill_name` must match `SKILL.md` frontmatter `name`.

## Tips

- Start with 2–4 checks per eval case; add more as you discover failure modes.
- Write checks that fail for the `without_skill` scenario but pass for
  `with_skill`. That is the signal your skill adds value.
- A `skill_used` grader is not that signal: it is settled by whether the skill
  was staged, so it is reported in both arms and scored in neither. When the
  point of the case is that the skill must *not* be engaged, write it as
  `{"type": "skill_used", "negate": true, "arm": "both"}`, because failing it is
  then the finding.
- `{"type": "tool_used", "tool": "Bash"}` asserts that a shell ran, which is
  rarely the point. Add `"input_match"` to name the command, as in
  `{"type": "tool_used", "tool": "Bash", "input_match": "npm (run )?test"}`, so
  the case says which command it meant.
- To ask whether the skill gets reached for at all, add
  `"skill_disclosure": "unannounced"` to the case. Announced prompts name the
  skill, so the routing decision is made for the run rather than by it.
- Keep `expected_output` as a human-readable reference; graders are what
  actually get checked, not exact string matching against it.

## Generated artifacts

### grading.json (trimmed)

After `trg ai skills eval grade`, each run directory contains:

```json
{
  "assertion_results": [
    {
      "assertion": "file 'summary.md' exists",
      "passed": true,
      "evidence": "'.../workspace/summary.md' exists and holds 142 bytes",
      "grader": { "kind": "declarative" }
    },
    {
      "assertion": "The summary mentions total revenue for May",
      "passed": false,
      "evidence": "no file under the workspace mentions May",
      "grader": { "kind": "llm", "model": "gpt-4o" }
    }
  ],
  "summary": {
    "passed": 1,
    "failed": 1,
    "total": 2,
    "unsupported": 0,
    "pass_rate": 0.5
  }
}
```

Assertion results are also copied into `report.json` under `assertion_results`
after grading completes.
