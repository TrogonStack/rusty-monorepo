# Divergences from agentskills.io

Reference for skill authors who have read the
[agentskills.io evaluating-skills guide](https://agentskills.io/skill-creation/evaluating-skills)
and need to know exactly where `trg ai skills eval` differs, and why.

## Summary

`trg` follows the agentskills.io **manifest** shape (`evals/evals.json`, scenario
kinds, assertion strings) and emits **docs-compatible companion files**
(`benchmark.json`, `grading.json`, `timing.json`, `feedback.json`,
`comparison.json`). The session index `report.json` is a **superset** with
`trg`-specific metadata. Compatibility is gated on `schema_version`.

| Topic | agentskills.io | `trg` |
| ----- | -------------- | ----- |
| Workspace root | `iteration-N/eval-<id>/<scenario>/` | Canonical: `runs/run-###/workspace/`; docs layout mirrored under `iteration-N/` |
| Session index | Spec report | `report.json` superset (`trg.skills-eval.report.v1`) |
| Old skill scenario | `with_old_skill` (implied) | `old_skill` |
| CLI workflow | Implied pipeline | Explicit subcommands: `run`, `grade`, `benchmark`, `verify`, `init`, `feedback`, `compare` |
| Schema stability | Spec versioning | Pre-1.0 contract via `schema_version`; snapshot tests in repo |

---

## Workspace layout

**agentskills.io** places each scenario under:

```text
iteration-N/eval-<id>/with_skill/
iteration-N/eval-<id>/without_skill/
iteration-N/eval-<id>/old_skill/
```

**`trg`** keeps a stable, machine-friendly canonical tree and mirrors the docs
layout as aliases:

```text
<report_dir>/
├── report.json
├── runs/run-001/workspace/     ← canonical agent working directory
├── runs/run-001/transcript.jsonl
├── runs/run-001/timing.json
├── runs/run-001/grading.json   ← after eval grade
└── iteration-1/
    ├── eval-<slug>/with_skill/ → symlink to runs/run-001/workspace (Unix)
    └── benchmark.json
```

Each run record carries `paths.workspace` (canonical) and `mirror_path` (docs
path). Eval IDs are normalized to filesystem slugs (`eval_slug`); the original
ID stays in `eval_case_id`.

**Why:** Run IDs (`run-001`, …) stay stable for caching, CI annotations, and
cross-tool references. The `iteration-N/` tree lets humans and docs-oriented
tools browse results without learning the internal numbering scheme. Symlinks
avoid duplicating agent outputs.

With `--attempts N > 1`, mirror paths nest as
`iteration-N/eval-<slug>/<scenario>/attempt-K/`.

---

## Report shape (`report.json`)

**agentskills.io** describes per-run artifacts and aggregates spread across the
iteration tree.

**`trg`** centralizes session metadata in `report.json` and writes
docs-shaped files beside it:

| Artifact | Shape relative to spec | Writer |
| -------- | ---------------------- | ------ |
| `report.json` | **Superset** — iteration, attempts, cache, runner metadata, dimensions, summaries, optional `iteration_summary` | `eval run` (+ merge from grade/benchmark) |
| `benchmark.json` | Docs-compatible aggregate; `failed_runs_mode` documents runner-failure policy | `eval benchmark` |
| `grading.json` | Docs-compatible per-run grading | `eval grade` |
| `timing.json` | Docs-compatible run metrics | Runner / `eval run` |
| `feedback.json` | Docs-compatible reviewer notes | `eval feedback init` / manual |
| `comparison.json` | Docs-compatible blind comparison | `eval compare` |

Extensions in `report.json` not covered by the public spec include: suite
hashes, skill integrity (tamper detection), CI context (GitHub Actions),
`improvement_feedback`, and per-run `cache` metadata.

---

## Schema versioning

All JSON artifacts carry a `schema_version` string (for example
`trg.skills-eval.report.v1`). This is a **pre-1.0 contract**: field names and
semantics may evolve within a major version only when backward compatible;
breaking changes require a new version constant.

**Consumers must gate on `schema_version`.** Do not assume a field exists
because the agentskills.io guide mentions it — check the version your file
carries.

Planned example: structured assertion objects (see below) will appear only when
`schema_version >= trg.skills-eval.report.v2`.

### Backward-compatibility contract

The repo commits frozen `report.json` fixtures under
`crates/trg/src/agentskills/testdata/reports/` and tests that:

1. Each fixture deserializes into the current `ReportDocument` struct.
2. Serialize → deserialize round-trip preserves every field present in the fixture.
3. Both fixtures validate against `schemas/report.json.schema.json`.

If you depend on `report.json` programmatically, treat these tests as the
compatibility contract for `trg.skills-eval.report.v1`. A failing snapshot test
means either a bug or an intentional version bump — look for a new
`schema_version`.

---

## Old-skill name match

When comparing against `--old-skill-dir`, the old skill's `SKILL.md` `name`
must match the current skill by default. Mismatch is a validation error.

Opt out with `--allow-skill-name-mismatch` when you intentionally compare
renamed or forked skills.

**Why:** Baseline comparisons assume the same skill identity; mismatched names
usually indicate a configuration mistake.

---

## Failed-runner accounting

Runner failures (spawn error, timeout, missing result event, non-zero exit) set
run `status` to `failed` or `timeout` with `failure_kind: "runner"`. They are
**not** mixed into assertion pass rate by default.

`eval benchmark` exposes a separate **`failed` bucket** per scenario (timing
and token stats only — no assertion pass rate). Default mode is `bucket`
(`failed_runs_mode: "bucket"`). Alternatives:

| Mode | CLI flag | Effect |
| ---- | -------- | ------ |
| `bucket` | `--failed-runs bucket` (default) | Failed runs in `scenarios.*.failed`; excluded from pass rate |
| `exclude` | `--failed-runs exclude` | Failed runs omitted entirely |
| `zero` | `--failed-runs zero` | Failed runs counted as zero pass rate |

CI exit policy is separate: `--fail-on-runner-failure` (strict CI) fails the
command when any run has `status: failed` or `timeout`, even if assertions
would pass.

**Why:** Runner infrastructure failures should not silently dilute or inflate
skill quality metrics.

---

## Empty outputs handling

Prompts instruct agents to write deliverables under `outputs/`. If a run
completes but `outputs/` is empty (or only contains runner-generated
`final.md`), **`eval run` does not fail the run**. The report lists zero output
artifacts; grading may warn or fail assertions that require files.

**Why:** Distinguish "agent ran but produced nothing" from "agent binary
crashed". The latter is a runner failure; the former is an assertion/quality
signal.

---

## Assertion shape

**Today (`trg.skills-eval.report.v1`):** `evals/evals.json` assertions are
plain strings. Mechanical grading matches natural-language patterns; LLM grading
handles the rest.

**Future (`schema_version >= v2`):** Structured assertion objects (kind, target,
params) may be added. Plain strings will remain valid for v1 consumers.

---

## LLM grading

**agentskills.io** describes LLM and mechanical graders in the evaluation loop.

**`trg`** ships both:

| Mode | Flag | Behavior |
| ---- | ---- | -------- |
| Auto (default) | `--grader auto` | Mechanical patterns first; marks remaining assertions `needs_llm` |
| LLM | `--grader llm` | Built-in LLM grading for assertions mechanical rules cannot resolve |
| Script | `--grader script --grader-command CMD` | External verifier; JSON stdin/stdout contract |
| None | `--grader none` | Mechanical only |

Use `eval grade` after `eval run`. Arbitrary external graders integrate via
`--grader script` without patching `trg`.

---

## CLI naming and workflow

| agentskills.io concept | `trg` command / flag |
| ---------------------- | -------------------- |
| Run eval cases | `trg ai skills eval run` |
| Grade assertions | `trg ai skills eval grade` |
| Aggregate benchmarks | `trg ai skills eval benchmark` |
| Validate artifacts | `trg ai skills eval verify` |
| Scaffold eval manifest | `trg ai skills eval init` |
| Human review | `trg ai skills eval feedback` |
| Blind comparison | `trg ai skills eval compare` |
| Old skill baseline | `--old-skill-dir`, `--scenario old_skill` |
| Iteration | `--iteration N` (auto-detect next when omitted) |
| Repeated samples | `--attempts N` |

There is no single "run everything" command; compose `run` → `grade` →
`benchmark` explicitly (or wrap them in CI).

---

## Other intentional extensions

These are `trg` additions, not spec divergences:

- **Skill integrity** — SHA-256 before/after tamper detection on the skill
  directory during runner execution.
- **CI metadata** — Auto-captured GitHub Actions context in `report.ci`.
- **Prompt contract** — Versioned runner prompts (`PROMPT_CONTRACT_VERSION`);
  skill body not duplicated in prompt when symlinked (token efficiency).
- **Transcript format** — Raw runner stream-json (`transcript.jsonl`), not a
  normalized spec envelope.

---

## Related docs

- [Eval lifecycle](eval-lifecycle.md)
- [Reference: flags and artifact schemas](../reference/ai-skills-eval.md)
- [Run with-skill vs without-skill](../how-to/run-with-skill-vs-without-skill.md)
