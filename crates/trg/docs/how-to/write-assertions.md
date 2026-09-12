# Write eval assertions

Assertions are natural-language checks in `evals/evals.json` that a judge
evaluates against agent workspace output. Good assertions are specific,
observable, and independent of implementation details.

If a check is mechanical, declare a typed grader instead. Assertions are for
the judgments a machine cannot make on its own. See
[Prefer a typed grader when the check is mechanical](#prefer-a-typed-grader-when-the-check-is-mechanical).

## Where assertions live

```json
{
  "skill_name": "csv-analyzer",
  "evals": [
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

## Writing effective assertions

| Do | Don't |
| -- | ----- |
| Describe observable outcomes in the workspace | Require exact wording |
| One check per assertion | Bundle unrelated checks |
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

## Pair assertions with fixtures

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
  "assertions": [
    "The workspace contains exactly one combined output file",
    "The combined output includes rows from both input files"
  ]
}
```

Paths must be relative to the skill directory and must exist at validation
time.

## Prefer a typed grader when the check is mechanical

A prose assertion has to be interpreted before it can be evaluated. Under
`--grader auto`, grading recognizes a handful of mechanical phrasings and sends
everything else to the LLM judge, which costs a call and can disagree with
itself between runs. Anything you can state precisely belongs in `graders`
instead, where the verdict is deterministic and needs no credential:

```json
{
  "id": "analyze-sales",
  "prompt": "Analyze evals/files/sales.csv and write a summary.",
  "expected_output": "A markdown summary with revenue totals by month.",
  "files": ["evals/files/sales.csv"],
  "graders": [
    { "type": "file_exists", "path": "summary.md" },
    { "type": "contains", "text": "May", "target": { "file": "summary.md" } },
    { "type": "regex", "pattern": "\\$[0-9,]+", "target": { "file": "summary.md" } },
    { "type": "tool_used", "tool": "Read" }
  ],
  "assertions": [
    "The summary reads as a coherent narrative rather than a data dump"
  ]
}
```

`graders` requires `schema_version: 3` on the suite. Mixing the two is the
intended shape: typed graders for the facts, assertions for the judgments. See
[Graders](../reference/ai-skills-eval.md#graders) for the full grader list.

Only prose assertions appear in `dimensions.assertions` in `report.json`. A
grader-only case contributes no assertion dimensions, but still produces
`assertion_results` once graded.

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
- `graders` requires `schema_version: 3`; declaring it on an older
  `schema_version` is rejected.
- Duplicate eval case IDs are rejected.
- `skill_name` must match `SKILL.md` frontmatter `name`.

## Tips

- Start with 2–4 assertions per eval case; add more as you discover failure modes.
- Write assertions that fail for the `without_skill` scenario but pass for
  `with_skill`. That is the signal your skill adds value.
- Keep `expected_output` as a human-readable reference; graders use assertions,
  not exact string matching against `expected_output`.

## Generated artifacts

### grading.json (trimmed)

After `trg ai skills eval grade`, each run directory contains:

```json
{
  "schema_version": "trg.skills-eval.grading.v3",
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
