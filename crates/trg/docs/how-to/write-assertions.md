# Write eval assertions

An eval case checks its output with typed `graders`, with natural-language
`assertions`, or both. If a check is mechanical, declare a typed grader: the
verdict is deterministic and needs no judge. Reserve `assertions`, or an
explicit `{"type": "llm", ...}` grader, for the judgments a machine cannot
make on its own.

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

Only prose `assertions` appear in `dimensions.assertions` in `report.json`; a
grader-only case like this one contributes no assertion dimensions, but still
produces `assertion_results` once graded. See
[Graders](../reference/ai-skills-eval.md#graders) for the full grader list and
the `target` values.

## Where `assertions` still fits

`assertions` is the lighter-weight surface it has always been: a plain string
instead of a typed object, graded mechanically when it happens to match a
known phrasing and handed to the LLM judge otherwise under `--grader auto`.
That fallback is implicit, which is exactly what declaring `llm` explicitly
avoids, so reach for a bare assertion for a suite you are not otherwise
touching, or for a one-off judgment call not worth spelling out as a grader:

```json
{
  "id": "analyze-sales",
  "prompt": "Analyze evals/files/sales.csv and write a summary.",
  "expected_output": "A markdown summary with revenue totals by month.",
  "files": ["evals/files/sales.csv"],
  "assertions": [
    "The workspace contains a summary file (summary.md or report.md)",
    "The summary mentions total revenue for May",
    "The summary includes a table or list of monthly figures"
  ]
}
```

Each assertion becomes a dimension entry in `report.json`:

```json
{
  "id": "analyze-sales:a0",
  "eval_case_id": "analyze-sales",
  "text": "The workspace contains a summary file (summary.md or report.md)"
}
```

Assertion IDs follow the pattern `<eval-case-id>:a<index>` (zero-based).

## Writing effective checks

| Do | Don't |
| -- | ----- |
| Describe observable outcomes in the workspace | Require exact wording |
| One check per assertion or grader | Bundle unrelated checks |
| Name acceptable file patterns (`summary.md` or `report.md`) | Hard-code a single filename the agent might not choose |
| Reference domain facts from fixtures ("May revenue") | Repeat the entire prompt |

### Example: strong vs weak

**Weak** (too vague, hard to grade consistently):

```json
"assertions": ["The output is good"]
```

**Strong** (checks concrete artifacts):

```json
"assertions": [
  "A CSV or markdown file in the workspace contains a row or line for May with revenue 10000",
  "No error messages appear in any file the agent created"
]
```

Both of these are mechanical once stated this precisely: the first is a
`regex` or `contains` grader against the known revenue figure, and the second
is a negated `regex` for an error pattern. Writing them as assertions still
works, but it defers to the judge a decision a grader could make for free.

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

Neither check has a typed equivalent: no grader counts the files a run
produced, or reconciles two source files against one output, so both stay
with the judge. That is the deliberate case for `llm`, distinct from the
worked example above: not "this could be mechanical but isn't written that
way yet," but "trg has no mechanism that answers this."

Paths must be relative to the skill directory and must exist at validation
time.

## How grading consumes assertions

`trg ai skills eval grade <report-dir>` (or `eval run --grade`) writes one
`grading.json` per run, with one entry per grader and per assertion:

```json
{
  "assertion": "The workspace contains a summary file",
  "passed": true,
  "evidence": "'.../workspace/summary.md' exists and holds 142 bytes",
  "grader": { "kind": "declarative" }
}
```

`evidence` must be a concrete observation. Grading rejects evidence that merely
restates the assertion, because a passing result nobody can check is worse than
no result.

Verify a graded bundle with:

```shell
trg ai skills eval verify ./runs/run-001/workspace --mode strict
```

## Validation rules

- Assertions and graders are both optional by default. Pass
  `--require-assertions` (or verify with `--mode strict`) to require that each
  case declares at least one of the two.
- Each assertion string must be non-empty.
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
- Keep `expected_output` as a human-readable reference; graders and assertions
  are what actually get checked, not exact string matching against it.

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
