# Write eval assertions

Assertions are natural-language checks in `evals/evals.json` that graders
evaluate against agent workspace output. Good assertions are specific,
observable, and independent of implementation details.

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

**Weak** — too vague, hard to grade consistently:

```json
"assertions": ["The output is good"]
```

**Strong** — checks concrete artifacts:

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

## How graders consume assertions

> **Status: planned** — automatic LLM grading that writes `grading.json` is not
> yet wired into `eval run`. Today, assertions are validated at suite load time
> and recorded in `report.json` dimensions.

When grading lands, each assertion will produce a `grading.json` entry:

```json
{
  "text": "The workspace contains a summary file",
  "passed": true,
  "evidence": "Found summary.md (142 lines)"
}
```

You can write `grading.json` manually today and verify with:

```shell
$ trg ai skills eval verify ./runs/run-001/workspace --mode strict
```

## Validation rules

- Assertions are optional at suite validation time (default).
- Each assertion string must be non-empty.
- Duplicate eval case IDs are rejected.
- `skill_name` must match `SKILL.md` frontmatter `name`.

## Tips

- Start with 2–4 assertions per eval case; add more as you discover failure modes.
- Write assertions that fail for the `without_skill` scenario but pass for
  `with_skill` — that is the signal your skill adds value.
- Keep `expected_output` as a human-readable reference; graders use assertions,
  not exact string matching against `expected_output`.

## Generated artifacts

### grading.json (trimmed)

After `trg ai skills eval grade`, each run directory contains:

```json
{
  "schema_version": "trg.skills-eval.grading.v1",
  "assertion_results": [
    {
      "assertion": "The workspace contains a summary file (summary.md or report.md)",
      "passed": true,
      "evidence": "Found summary.md",
      "grader": { "kind": "mechanical" }
    },
    {
      "assertion": "The summary mentions total revenue for May",
      "passed": false,
      "evidence": "No file mentions May revenue",
      "grader": { "kind": "mechanical" }
    }
  ],
  "summary": {
    "passed": 1,
    "failed": 1,
    "total": 2,
    "pass_rate": 0.5
  }
}
```

Assertion results are also copied into `report.json` under `assertion_results`
after grading completes.
