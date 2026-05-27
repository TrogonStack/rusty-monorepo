# Write mechanical grader scripts

Mechanical graders check deterministic properties of the agent workspace —
file existence, content patterns, exit codes — without calling an LLM. Use
them for fast, cheap, reproducible checks alongside (or instead of) LLM grading.

## Current state

> **Status: planned** — `trg` does not yet invoke grader scripts automatically.
> On `yordis/eval-2` you run scripts manually after `eval run` completes and
> write the results into `grading.json` for `eval verify` to validate.

The `dimensions.graders` array in `report.json` is scaffolded empty.

## Recommended pattern

### 1. Run the eval

```shell
$ trg ai skills eval run \
    --skill-dir ./skills/csv-analyzer \
    --out-dir ./artifacts \
    --runner cursor-agent
./artifacts/csv-analyzer/20260526T170000Z-deadbeef
```

### 2. Grade each run workspace

Point your script at the workspace directory and the eval case assertions:

```shell
$ ./skills/csv-analyzer/evals/grade.sh \
    ./artifacts/csv-analyzer/20260526T170000Z-deadbeef/runs/run-001/workspace \
    analyze-sales
Wrote grading.json (2/3 passed)
```

### 3. Verify the grading output

```shell
$ trg ai skills eval verify \
    ./artifacts/csv-analyzer/20260526T170000Z-deadbeef/runs/run-001/workspace \
    --mode strict
Bundle verified
  grading files: 1
  timing files: 1
  assertion results: 2/3 passed (66.67%)
```

## Example grader script

Place under `evals/grade.sh` in the skill directory:

```bash
#!/usr/bin/env bash
set -euo pipefail

WORKSPACE="${1:?workspace path}"
EVAL_ID="${2:?eval case id}"

# Load assertions for this eval case from evals.json
ASSERTIONS=$(jq -r --arg id "$EVAL_ID" \
  '.evals[] | select(.id == $id) | .assertions[]' \
  evals/evals.json)

PASSED=0
FAILED=0
RESULTS="[]"

check_file_exists() {
  local pattern="$1"
  local text="$2"
  if compgen -G "${WORKSPACE}/${pattern}" > /dev/null; then
    PASSED=$((PASSED + 1))
    RESULTS=$(echo "$RESULTS" | jq --arg t "$text" \
      '. + [{"text": $t, "passed": true, "evidence": "matching file found"}]')
  else
    FAILED=$((FAILED + 1))
    RESULTS=$(echo "$RESULTS" | jq --arg t "$text" \
      '. + [{"text": $t, "passed": false, "evidence": "no matching file"}]')
  fi
}

while IFS= read -r assertion; do
  case "$assertion" in
    *summary*file*)
      check_file_exists "*summary*" "$assertion"
      ;;
    *May*revenue*)
      if grep -rq "May" "$WORKSPACE" 2>/dev/null; then
        PASSED=$((PASSED + 1))
        RESULTS=$(echo "$RESULTS" | jq --arg t "$assertion" \
          '. + [{"text": $t, "passed": true, "evidence": "May mentioned in output"}]')
      else
        FAILED=$((FAILED + 1))
        RESULTS=$(echo "$RESULTS" | jq --arg t "$assertion" \
          '. + [{"text": $t, "passed": false, "evidence": "May not found"}]')
      fi
      ;;
    *)
      FAILED=$((FAILED + 1))
      RESULTS=$(echo "$RESULTS" | jq --arg t "$assertion" \
        '. + [{"text": $t, "passed": false, "evidence": "no mechanical rule matched"}]')
      ;;
  esac
done <<< "$ASSERTIONS"

TOTAL=$((PASSED + FAILED))
RATE=$(echo "scale=4; $PASSED / $TOTAL" | bc)

jq -n \
  --argjson results "$RESULTS" \
  --argjson passed "$PASSED" \
  --argjson failed "$FAILED" \
  --argjson total "$TOTAL" \
  --argjson rate "$RATE" \
  '{
    assertion_results: $results,
    summary: { passed: $passed, failed: $failed, total: $total, pass_rate: ($rate | tonumber) }
  }' > "${WORKSPACE}/grading.json"

echo "Wrote grading.json (${PASSED}/${TOTAL} passed)"
```

## `grading.json` contract

Your script must produce output that `eval verify` accepts:

| Rule | Detail |
| ---- | ------ |
| Location | `grading.json` inside the workspace (or nested subdirectories) |
| `assertion_results` | At least one entry; each needs non-empty `text` and `evidence` |
| `summary` | Counts must match the results array; `pass_rate = passed / total` |

See the [reference](../reference/ai-skills-eval.md#artifact-gradingjson) for the
full schema.

## Script design tips

- **Read-only workspace inspection** — never modify agent output during grading.
- **Deterministic** — same workspace always produces the same `grading.json`.
- **Fast** — mechanical checks should complete in milliseconds.
- **Composable** — one script per skill, parameterized by eval case ID.
- **Escape hatch** — return `"no mechanical rule matched"` for assertions that
  need LLM grading in a future pass.

## Future integration

> **Status: planned** — a future grader PR will register scripts in
> `evals/graders.json`, invoke them after each runner completes, and merge
> results into `report.json` `assertion_results`.
