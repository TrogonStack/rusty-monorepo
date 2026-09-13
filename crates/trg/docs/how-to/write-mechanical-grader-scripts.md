# Write mechanical grader scripts

Mechanical graders check deterministic properties of the agent workspace (file
existence, content patterns, tool usage) without calling an LLM. They are fast,
cheap, and reproducible.

## Prefer typed graders when they fit

Most mechanical checks no longer need a script. Declare them in the manifest
and `eval grade` evaluates them in process:

```json
{
  "id": "analyze-sales",
  "prompt": "Summarize monthly revenue from sales.csv",
  "graders": [
    { "type": "file_exists", "path": "summary.md" },
    { "type": "contains", "text": "May", "target": { "file": "summary.md" } },
    { "type": "tool_used", "tool": "Read" }
  ]
}
```

See [Graders](../reference/ai-skills-eval.md#graders) for the full grader list
and the `target` values. Reach for a script only when the check needs something
the typed graders cannot express: running the produced artifact, parsing a
domain format, or calling out to another tool.

## Wire a script into `eval grade`

`--grader script` invokes your command once per assertion, with the working
directory set to the run workspace:

```shell
$ trg ai skills eval grade ./artifacts/csv-analyzer/20260526T170000Z-deadbeef \
    --grader script \
    --grader-command ./skills/csv-analyzer/evals/grade.sh
```

### Input on stdin

```json
{
  "assertion": "The workspace contains a summary file",
  "workspace": "/abs/path/runs/run-001/workspace",
  "outputs": "/abs/path/runs/run-001/workspace/outputs",
  "transcript": "/abs/path/runs/run-001/transcript.jsonl",
  "grader_hints": { "expected_month": "May" }
}
```

`grader_hints` appears only when the eval case declares it.

### Output on stdout

```json
{
  "passed": true,
  "evidence": "summary.md exists and holds 412 bytes",
  "rationale": "optional free text"
}
```

| Rule | Detail |
| ---- | ------ |
| Exit code | Non-zero marks the assertion failed and records stderr as the evidence |
| Stdout | Must be a single JSON object; anything else is a validation error |
| `evidence` | Must be a concrete observation, not a restatement of the assertion |
| Artifact | Each invocation writes `grader-script-result.json` into the run directory with the exit code, stdout, and stderr |

### Example script

Place it under `evals/grade.sh` in the skill directory:

```bash
#!/usr/bin/env bash
set -euo pipefail

INPUT=$(cat)
ASSERTION=$(jq -r '.assertion' <<< "$INPUT")
WORKSPACE=$(jq -r '.workspace' <<< "$INPUT")

emit() {
  jq -n --argjson passed "$1" --arg evidence "$2" \
    '{passed: $passed, evidence: $evidence}'
}

case "$ASSERTION" in
  *summary*file*)
    MATCH=$(find "$WORKSPACE" -maxdepth 2 -name '*summary*' -type f | head -1)
    if [[ -n "$MATCH" ]]; then
      emit true "found $(basename "$MATCH") at $(wc -c < "$MATCH" | tr -d ' ') bytes"
    else
      emit false "no file matching *summary* under $WORKSPACE"
    fi
    ;;
  *May*revenue*)
    HITS=$(grep -rc "May" "$WORKSPACE" 2>/dev/null | awk -F: '{s+=$2} END {print s+0}')
    if [[ "$HITS" -gt 0 ]]; then
      emit true "\"May\" appears $HITS times across the workspace"
    else
      emit false "\"May\" does not appear anywhere under $WORKSPACE"
    fi
    ;;
  *)
    emit false "no mechanical rule matched this assertion"
    ;;
esac
```

## Writing `grading.json` yourself

`eval verify` also accepts a `grading.json` that you produce out of band, which
is the route to take when one pass over the whole workspace is cheaper than one
invocation per assertion:

```shell
$ ./skills/csv-analyzer/evals/grade-all.sh \
    ./artifacts/csv-analyzer/20260526T170000Z-deadbeef/runs/run-001/workspace \
    analyze-sales
Wrote grading.json (2/3 passed)

$ trg ai skills eval verify \
    ./artifacts/csv-analyzer/20260526T170000Z-deadbeef/runs/run-001/workspace \
    --mode strict
Bundle verified
  grading files: 1
  timing files: 1
  assertion results: 2/3 passed (66.67%)
```

### `grading.json` contract

| Rule | Detail |
| ---- | ------ |
| Location | `grading.json` inside the workspace (or a nested subdirectory) |
| `schema_version` | `trg.skills-eval.grading.v5`; `v4`, `v3`, `v2` and `v1` still read |
| `assertion_results` | At least one entry; each needs a non-empty `assertion` (`text` is accepted as an alias) and `evidence` |
| `summary` | `passed`, `failed`, `total`, and `unsupported` must match the results array |
| `pass_rate` | `passed / (total - unsupported)`, or `null` when nothing was scored |

See the [reference](../reference/ai-skills-eval.md#artifact-gradingjson) for the
full schema.

## Script design tips

- **Read-only workspace inspection.** Never modify agent output during grading.
- **Deterministic.** The same workspace must always produce the same verdict.
- **Fast.** Mechanical checks should complete in milliseconds.
- **Concrete evidence.** Report byte counts, offsets, paths, and match counts.
  Grading rejects evidence that merely restates the assertion.
- **Escape hatch.** Fail with `"no mechanical rule matched"` for assertions that
  belong to the LLM judge, and grade those with `--grader auto` instead.
