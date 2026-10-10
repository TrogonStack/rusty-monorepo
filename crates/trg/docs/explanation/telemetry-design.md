# Telemetry design

Why `trg` emits OpenTelemetry the way it does: which conventions it follows,
how an eval pass becomes a trace, and why the defaults are as conservative as
they are. For the exact names, see the
[telemetry reference](../reference/telemetry.md).

## Standard conventions over a private schema

`trg` sits between things that already have OpenTelemetry vocabularies: an MCP
host and an MCP server, an eval harness and a model provider, a judge and an
LLM API. Naming spans and attributes after the GenAI and MCP semantic
conventions means a backend that understands those conventions can show a
`trg` trace next to the agent's own spans and the provider's, without a
translation layer. An `invoke_agent` span from `trg` and one from a harness
that exports natively describe the same thing in the same words.

The `trg.*` namespace covers only what no convention names: eval cases,
scenarios, cache hits, secrets backends, the reason a proxy session ended.
Those are concepts that exist because of `trg`, and keeping them in their own
namespace avoids claiming a meaning upstream never agreed to.

The GenAI and MCP names are generated from a pinned commit of the upstream
conventions rather than typed by hand. Most of them are still marked as
development upstream, and pinning makes every rename a deliberate change
rather than drift.

## How an eval pass maps to a trace

An eval pass is a workflow that invokes agents, so its shape follows the GenAI
agent conventions:

```text
trg
└── invoke_workflow <skill>
    ├── validate skill, check suite, ...
    └── execute runs
        └── run                      (one per case, scenario, and draw)
            └── attempt              (one per retry)
                ├── prepare workspace
                └── invoke_agent <harness>
                    ├── chat <model>
                    └── execute_tool <tool>
```

**Cases are attributes, not spans.** A case is a label on its runs, not a unit
of work with its own duration. Putting `trg.eval.case.id` and
`trg.eval.scenario` on each `run` lets a backend group and compare runs by
case without inventing a span that would only ever wrap them.

**Lanes keep concurrency honest.** With `-j`, runs execute on separate lanes
but stay children of `execute runs`, carrying `trg.eval.lane`. The trace
shows real overlap instead of a sequence that never happened.

**Grading is a different trace, joined by links.** `eval grade` and
`eval compare` usually run in a later process, sometimes on another machine.
Making them children of a run span that ended long ago would distort every
duration in the tree. Instead, `report.json` records the suite and run span
ids, and grading spans link to them. Evaluation events are emitted in the run
span's context, so a backend shows a verdict on the run it judged while the
grading work keeps its own timeline.

**Verdicts are logs, not span events.** A verdict's explanation quotes what
was graded, so it is content. Attached to a span it would land on the trace
whatever the content capture mode allows there. Sending evaluation events on
the logs signal alone keeps trace content and event content under separate
switches, while the record's trace context still ties it to the run.

**A finished bundle can still become a trace.** A pass that ran with tracing
off leaves a complete `report.json`, and `eval export` rebuilds that pass as a
trace after the fact. The replay reuses the live span names so the same
queries work, and marks everything with `trg.eval.replayed` so a backend never
mistakes it for live data. It skips runs already traced live by default,
because counting their usage and cost twice would be worse than a gap. See
[Replay a report bundle into a telemetry backend](../how-to/replay-a-report-bundle.md).

## Off by default, and quiet when on

A CLI that phones home by default would surprise people, and `trg` often runs
inside editors and CI where an unexpected network call matters. Export starts
only when an OTLP endpoint is configured through the standard variables, which
are the same ones every other OpenTelemetry SDK reads. There is no `trg`
specific switch to learn.

Telemetry never writes to stdout. `trg mcp proxy` speaks JSON-RPC on stdout,
and a single stray line there breaks the host. Runtime exporter errors, such
as a collector that stopped answering, go to the local file log only, because
they can recur on every batch and would bury a command's own output.

A misconfiguration is different: asking for gRPC or a `console` exporter means
the person running `trg` expects telemetry they will never get. That is
reported once per process as a single stderr line, which is safe for every
command and visible to whoever set the variable. Without it, the only sign
would be a quiet backend.

`trg` exports over http/protobuf only. That keeps a gRPC stack out of a CLI
binary, and HTTP reaches every collector and most backends directly.

## No content by default

The default trace answers "what happened and how long did it take": which
tool, which model, how many tokens, which error. It does not answer "what was
said". Tool arguments and results, judge prompts, and grader evidence can hold
source code, customer data, and credentials a tool echoed back, and none of
that belongs in a telemetry backend unless someone chose to put it there.

Capture follows the opt-in used by the OpenTelemetry Python GenAI
instrumentation: a stability opt-in plus a capture mode. Requiring both makes
it hard to turn on by accident, and reusing an existing convention means the
same variables work for other instrumented tools in the same process tree.
See [Capture GenAI message content](../how-to/capture-genai-message-content.md).

The same caution applies to attributes that are not message content. The root
span leaves out command arguments, secrets spans record the backend name and
the fixed subcommand rather than paths or arguments, and HTTP spans strip query
strings and credentials from URLs.

## Getting telemetry out before the process goes away

Most `trg` commands are short, and telemetry is batched. Without care, a fast
command would exit before anything was sent. `trg` flushes on exit with a
bounded wait, so a dead collector delays exit by a few seconds at most instead
of hanging it.

Some exits need more than that:

- **Signals.** When `trg` is interrupted, it ends the root span with the exit
  code the shell will report and an `error.type` naming the signal, flushes
  briefly, and re-raises the signal so the parent still sees a signal death.
  An interrupted eval pass shows up as interrupted rather than vanishing.
- **`exec`.** `trg exec` replaces itself with the target program. Nothing runs
  after `exec(2)`, so `trg` ends its root span and flushes first, and passes
  `TRACEPARENT` to the target so a program that understands it continues the
  same trace.

## Crossing process boundaries

`trg` starts many processes: harnesses, graders, secrets CLIs, mock servers.
Each one gets `TRACEPARENT`, `TRACESTATE`, and `BAGGAGE` pointing at the span
that started it, so anything instrumented joins the trace without extra setup.
These variables are only set when a trace is actually being exported, so a
child never receives a context that points at nothing.

Exporter settings are a different matter. Handing a harness `OTEL_*` variables
lets it reach the network on its own, which an isolated run exists to prevent.
That is why forwarding exporter settings to harnesses waits for
`--forward-telemetry` and is recorded in the report. See
[Forward telemetry into eval harnesses](../how-to/forward-telemetry-to-eval-harnesses.md).

## Related

- [Telemetry reference](../reference/telemetry.md)
- [Export telemetry over OTLP](../how-to/export-telemetry-over-otlp.md)
- [Eval lifecycle](eval-lifecycle.md)
