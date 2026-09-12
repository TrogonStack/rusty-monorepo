# Troubleshooting skill evals

Common failures when running `trg ai skills eval` and how to resolve them.

---

## Runner setup

### `failed to spawn 'cursor-agent'`

The runner binary is not on `PATH`.

```shell
$ trg ai skills eval run --skill-dir ./skill --out-dir ./out --runner cursor-agent
Run run-001 failed: failed to spawn 'cursor-agent': No such file or directory
```

**Fix:** Install the runner CLI and confirm it is executable:

```shell
$ which cursor-agent
/usr/local/bin/cursor-agent

$ cursor-agent --version
```

Supported runners: `cursor-agent`, `claude-code` (spawns `claude`), `codex`.

### `'cursor-agent' exited without emitting a terminal result event`

The runner produced stdout but no stream-json `{"type":"result",...}` line.

```shell
Run run-001 failed: 'cursor-agent' exited without emitting a terminal result event
```

**Fix:**

- Confirm the runner supports `--output-format stream-json`.
- Check `runs/run-001/transcript.jsonl` for partial output.
- Retry with `--runner-model` if the default model is unavailable.

### `--old-skill-dir is required when --scenario old_skill is included`

The `old_skill` scenario stages a prior skill revision into the workspace, so it
needs to know which revision.

**Fix:** Pass `--old-skill-dir ./path/to/previous/skill`, or drop
`--scenario old_skill`.

### `Old skill name 'X' does not match current skill name 'Y'`

A name mismatch usually means `--old-skill-dir` points at a different skill
rather than an earlier revision of the same one.

**Fix:** Point at the right directory, or pass `--allow-skill-name-mismatch`
when the rename is intentional.

---

## Skill and eval validation

### `Skill validation failed`

`SKILL.md` frontmatter is missing or invalid.

**Fix:** Run `trg ai skills validate --skill-dir ./skill` for details.

### `Skill eval validation failed: 'foo' must match skill frontmatter name 'bar'`

`evals.json` `skill_name` does not match `SKILL.md` `name`.

**Fix:** Align the names:

```yaml
# SKILL.md frontmatter
name: csv-analyzer
```

```json
{ "skill_name": "csv-analyzer", ... }
```

### `path '../outside.csv' must stay inside the skill directory`

A `files` entry in `evals.json` escapes the skill tree.

**Fix:** Use paths relative to the skill root without `..` segments.

### `path 'evals/files/missing.csv' does not exist`

A staged fixture file is missing.

**Fix:** Create the file or remove it from the eval case `files` array.

---

## Report bundle errors

### `report directory already exists` (pass --force to overwrite)

A previous run wrote to the same report ID.

**Fix:** Use `--force` to overwrite, or delete the existing directory:

```shell
trg ai skills eval run ... --force
```

Report IDs include a timestamp and random suffix, so collisions are rare unless
you re-run within the same second with identical entropy.

---

## Missing grading artifacts

### `verify` reports `grading files: 0`

No `grading.json` found under the workspace.

```shell
$ trg ai skills eval verify ./runs/run-001/workspace
Bundle verified
  grading files: 0
  timing files: 1
  assertion results: 0/0 passed (0.00%)
```

**Fix:**

- Grade the bundle: `trg ai skills eval grade <report-dir>`, or add `--grade` to
  `eval run` so grading happens in the same invocation.
- If the suite declares no assertions and no graders, nothing can be graded.
  Add at least one of the two to each eval case.
- To grade out of band instead, write `grading.json` yourself or via a grader
  script (see
  [write-mechanical-grader-scripts.md](./write-mechanical-grader-scripts.md)).

### `must contain at least one grading.json` (strict mode)

```shell
$ trg ai skills eval verify ./workspace --mode strict
Bundle verification failed: workspace '...' must contain at least one grading.json
```

**Fix:** Run your grader before verify, or switch to `--mode lenient` during
development.

### `summary.passed does not match N passed assertion results`

The `grading.json` summary counts are inconsistent with `assertion_results`.

**Fix:** Recompute summary fields:

```text
passed      = count of scored assertion_results where passed == true
failed      = count of scored results where passed == false
unsupported = count of results carrying `unsupported`
total       = len(assertion_results)
pass_rate   = passed / (total - unsupported), or null when nothing was scored
```

---

## Permission errors

### `Failed to write eval report bundle: Permission denied`

**Fix:** Check write permissions on `--out-dir`:

```shell
mkdir -p ./artifacts && trg ai skills eval run --out-dir ./artifacts ...
```

---

## Skill integrity (tampering)

After a runner completes, `report.json` may include:

```json
{
  "skill_integrity": {
    "tampered": true,
    "tampered_files": ["SKILL.md", "evals/evals.json"]
  }
}
```

The agent modified skill source files during the run. This is informational:
the run is not automatically failed.

**Fix:** Review whether the agent should write to the skill directory. Scope
agent writes to the workspace only.

---

## Timing validation

### `duration_ms must be greater than zero`

A malformed `timing.json` was written.

**Fix:** Ensure `duration_ms` is a positive integer. Runners write this
automatically; manual edits must respect the constraint.

---

## Quick diagnostic checklist

| Symptom | Check |
| ------- | ----- |
| All runs `skipped` | Did you pass `--runner`? |
| All runs `failed` | Is the runner CLI installed and authenticated? |
| Empty workspace after run | Runner may have failed before writing output; check stderr |
| `pass_rate: null` | Nothing was scored: every result is `unsupported`, or there is no `grading.json` |
| Duplicate scenario error | Remove duplicate `--scenario` flags |
| CI missing context in report | Set `GITHUB_ACTIONS=true` (automatic on GitHub Actions) |

For artifact schemas and flag reference, see
[ai-skills-eval reference](../reference/ai-skills-eval.md).
