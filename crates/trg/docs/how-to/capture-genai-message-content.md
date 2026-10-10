# Capture GenAI message content

By default `trg` exports the shape of every model call and tool call (names,
models, token counts, durations, errors) and none of what was said. This guide
turns on content capture for a debugging session and explains what that puts
in your telemetry backend.

It assumes export already works. If not, start with
[Export telemetry over OTLP](export-telemetry-over-otlp.md).

## Turn it on

Content capture needs each of these variables, set exactly as shown:

```sh
export OTEL_SEMCONV_STABILITY_OPT_IN="${OTEL_SEMCONV_STABILITY_OPT_IN:+$OTEL_SEMCONV_STABILITY_OPT_IN,}gen_ai_latest_experimental"
export OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=SPAN_ONLY
```

- `OTEL_SEMCONV_STABILITY_OPT_IN` is a comma-separated list. It must contain
  `gen_ai_latest_experimental`; other entries are left alone. The command
  above appends it to any value you already set.
- `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT` must be one of
  `SPAN_ONLY`, `EVENT_ONLY`, or `SPAN_AND_EVENT`, in upper case. Any other
  value, including `true` or `span_only`, captures nothing.

If either variable is missing or off, nothing is captured, and `trg` does not
warn about it.

## Pick a mode

| Mode | What it adds |
| ---- | ------------ |
| `SPAN_ONLY` | Message and tool content as attributes on spans |
| `EVENT_ONLY` | The grader's explanation on each `gen_ai.evaluation.result` event |
| `SPAN_AND_EVENT` | Both |

Span content covers:

- `trg mcp proxy`: `gen_ai.tool.call.arguments` and `gen_ai.tool.call.result`
  on each `tools/call` span.
- LLM judges in `eval grade` and `eval compare`: `gen_ai.system_instructions`,
  `gen_ai.input_messages`, and `gen_ai.output_messages` on each `chat` span.
- `eval record-mcp`: tool arguments and results on each `tools/call` span.
- Eval runs where `trg` rebuilds the harness's tool calls from its output:
  arguments and results on each `execute_tool` span.

Event content is the `gen_ai.evaluation.explanation` attribute, which quotes
the evidence a grader or comparison judge based its verdict on. It is emitted
by `eval grade`, `eval compare`, and `eval export` when it replays grading
from a report bundle. Evaluation events are sent on the logs signal only,
never attached to spans, so under `EVENT_ONLY` the explanation reaches your
logs backend and not your trace backend.

## Know what leaves the machine

Captured content is whatever flowed through the call, unfiltered. Expect:

- Tool arguments and results from MCP servers behind `trg mcp proxy`, which
  can include file contents, query results, tokens a tool echoed back, and
  anything else the server returned.
- The full prompt and response of every LLM judge call, which quotes the
  agent's transcript and output files.
- Grader evidence, which quotes the run's output.

`trg` redacts credentials it knows about from URLs and from what it records in
eval artifacts, but it does not inspect captured content. Anyone who can read
your telemetry backend can read that content, and the backend's retention
applies to it.

Turn content capture on for a bounded session against a collector you
control. Leave it off in shared or long-lived pipelines and in CI.

## Know what else changes

Content capture also changes what `trg` hands to harnesses when
`--forward-telemetry` is on. Any mode, including `EVENT_ONLY`, asks the
harness to log prompts as well:

- Claude Code gets `OTEL_LOG_USER_PROMPTS=1` and `OTEL_LOG_TOOL_DETAILS=1`.
- Codex gets `log_user_prompt = true` in its `[otel]` table.

See [Forward telemetry into eval harnesses](forward-telemetry-to-eval-harnesses.md).

The local file log (`trg.log` under `$XDG_CACHE_HOME/trg` or `~/.cache/trg`)
is written by the same instrumentation and can also hold captured content
while capture is on. Treat it with the same care.

## Turn it off

Unset either variable:

```sh
unset OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT
```

Telemetry already exported stays in your backend. Delete it there if needed.

## Related

- [Telemetry reference](../reference/telemetry.md#content-capture)
- [Telemetry design](../explanation/telemetry-design.md)
