# Replay a report bundle into a telemetry backend

Send a finished eval pass to your telemetry backend after the fact, as the
trace a live `eval run` would have exported. `report.json` is read and never
modified.

Use this when:

- the pass ran with tracing off, for example in a CI job with no collector
- the backend has expired the live trace and you want the pass back in it
- you want a backend to hold the verdicts of a pass graded with tracing off

To trace a pass as it runs instead, see
[Export telemetry over OTLP](export-telemetry-over-otlp.md).

## Before you start

- A report bundle: a directory holding `report.json`, as written by
  `eval run`.
- An OTLP endpoint for traces, set through the standard variables. Without
  one, `eval export` prints that nothing was exported and exits `0`. Logs are
  needed too if you want the evaluation events.

## Replay the bundle

Point the standard variables at your collector and pass the bundle directory:

```sh
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 \
  trg ai skills eval export ./artifacts/csv-analyzer/20260526T120000Z-abc
```

`trg` prints the id of the new trace and how many runs it replayed:

```text
replayed trace: 4bf92f3577b34da6a3ce929d0e0e4736
runs replayed: 2
```

Search your backend for that trace id. It holds an `invoke_workflow {skill}`
root span with `execute runs`, a `run` span per replayed run, and, when the
bundle was graded, a `grade` span with one `grade run` span per graded run.
Evaluation results arrive as `gen_ai.evaluation.result` log records in the
context of the replayed run span.

For a step in a script, ask for JSON and read the span references from it:

```sh
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 \
  trg ai skills eval export ./artifacts/csv-analyzer/20260526T120000Z-abc --output-format json
```

`outcome` is `exported`, `nothing_to_replay`, or `tracing_off`. The
[`eval export` reference](../reference/ai-skills-eval.md#eval-export) shows the
full document.

## Tell a replay from a live pass

Every replayed span and evaluation event carries `trg.eval.replayed = true`,
and the root span carries `trg.eval.report.id`. Live telemetry never sets
`trg.eval.replayed`. Filter on it to keep replays out of dashboards built for
live passes, or to find them:

```text
trg.eval.replayed = true AND trg.eval.report.id = "20260526T120000Z-abc"
```

The query syntax depends on your backend. Span names and the other attributes
match a live pass, so a query written against live traces also matches a
replayed one.

## Decide whether to replay runs already traced live

A run whose `trace` field in `report.json` is set was exported when it ran.
By default `eval export` skips such runs, so a backend that still holds the
live trace does not count their token usage and cost twice. The text output
says how many it skipped, and the JSON lists them under `skipped_runs`.

Grading is decided separately, from each run's `grade_trace` field:

- A run graded under a live trace has its grading skipped and listed under
  `skipped_grades`, since its evaluation events were already sent.
- A run traced live but graded with tracing off keeps its live run span. Only
  its grading is replayed, as a child of that live run span, so it lands in
  the live trace rather than the new one. The JSON lists it under
  `live_run_grades` with where its `grade run` span landed.

When the backend no longer holds the live trace, replay everything with
`--include-traced`. Each replayed span then links to the live span it
duplicates:

```sh
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 \
  trg ai skills eval export ./artifacts/csv-analyzer/20260526T120000Z-abc --include-traced
```

If every run and every grade was already traced live and you leave out
`--include-traced`, nothing is exported.

## Know what a replay cannot show

A report records less than a live trace, so a replay is an approximation:

- **Timing.** A report records when the pass started, `report.generated_at`,
  and how long each run took, but not when each run started. Every run starts
  at `report.generated_at`, so runs of a serial pass appear to overlap. Grading
  time is not recorded either, so grading spans are zero-length.
- **Metrics.** None are replayed. An OTLP data point is stamped when it is
  exported, so replayed counters would attribute old runs and cost to the
  moment of the export.
- **Detail.** Setup steps, retried attempts before the final one, harness
  turns, and tool calls are not in the report and do not appear.
- **Identity.** Span ids are new on every export. Exporting the same bundle
  twice produces two traces, so replay a bundle once per backend.

Evaluation events carry the grader's evidence only under the content capture
settings a live `grade` honors. See
[Capture GenAI message content](capture-genai-message-content.md).

## Related

- [`eval export` reference](../reference/ai-skills-eval.md#eval-export)
- [Telemetry reference](../reference/telemetry.md)
- [Telemetry design](../explanation/telemetry-design.md)
- [Export telemetry over OTLP](export-telemetry-over-otlp.md)
