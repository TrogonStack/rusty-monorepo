# Forward telemetry into eval harnesses

Make the agent CLI that `trg ai skills eval run` drives export its own spans,
metrics, and logs to your collector, nested under the eval run's trace. Use it
when the harness's own view (its model calls, tool calls, and token
accounting) matters more than what `trg` can rebuild from the harness output.

It assumes `trg` itself already exports. If not, start with
[Export telemetry over OTLP](export-telemetry-over-otlp.md).

## Decide whether you need it

Without forwarding, `trg` still traces every run: the `invoke_agent` span
wraps the harness process, and `trg` rebuilds `chat` and `execute_tool` spans
from what the harness printed. That is often enough.

Forwarding is off by default because it opens a door the run environment
otherwise keeps shut. Under `--environment isolated` or `scrubbed`, a run sees
none of your `OTEL_*` variables, so it cannot reach your collector. With
`--forward-telemetry`, it can. The harness then decides what to send,
including, if asked, prompts and tool output.

## Turn it on

```sh
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
trg ai skills eval run \
  --skill-dir ./skills/csv-analyzer \
  --out-dir ./artifacts \
  --runner claude-code \
  --forward-telemetry
```

`report.json` records the choice as `report.telemetry_forwarding: "on"`, so a
reader knows the runs could reach a collector. The field is absent when
forwarding was off.

With the `claude-code` runner, when a traces endpoint is configured and
`OTEL_TRACES_EXPORTER` is not `none`, Claude Code exports its own spans and
`trg` stops rebuilding `chat` and `execute_tool` spans for that run, so nothing
appears twice. In every other case `trg` keeps rebuilding them from the
harness transcript.

## What each harness receives

### `claude-code`

Under `isolated` and `scrubbed`, these host variables are let through:

- `TRACEPARENT`, `TRACESTATE`, `BAGGAGE`
- every `OTEL_*` variable except `OTEL_SERVICE_NAME`, `OTEL_LOG_USER_PROMPTS`,
  `OTEL_LOG_TOOL_DETAILS`, and `OTEL_LOG_TOOL_CONTENT`

Under `inherited`, the host environment already passes through untouched,
including any content switches you set yourself.

On top of that, under every policy, `trg` sets:

- `CLAUDE_CODE_ENABLE_TELEMETRY=1` and `CLAUDE_CODE_ENHANCED_TELEMETRY_BETA=1`
- `OTEL_TRACES_EXPORTER=otlp`, `OTEL_METRICS_EXPORTER=otlp`, and
  `OTEL_LOGS_EXPORTER=otlp` for each signal that has an endpoint, unless you
  already set that exporter yourself
- `CLAUDE_CODE_PROPAGATE_TRACEPARENT=1` when `ANTHROPIC_BASE_URL` is set, so
  a gateway in front of the provider joins the same trace
- `OTEL_LOG_USER_PROMPTS=1` and `OTEL_LOG_TOOL_DETAILS=1` when `trg` content
  capture is on, in any mode. See
  [Capture GenAI message content](capture-genai-message-content.md)

`OTEL_SERVICE_NAME` is held back so the harness reports under its own service
name instead of claiming to be `trg`.

### `codex`

Codex reads its exporters from the `[otel]` table of its `config.toml`, not
from environment variables. `trg` renders that table from your `OTEL_*`
variables and appends it to the run's own config home:

- `exporter` (logs), `trace_exporter`, and `metrics_exporter`, each set for a
  signal that has an endpoint and is not set to `none`
- `otlp-http` with `protocol = "binary"`, or `"json"` for `http/json`, or
  `otlp-grpc` when that signal's protocol is `grpc`. Codex supports gRPC even
  though `trg` does not
- a shared `OTEL_EXPORTER_OTLP_ENDPOINT` gets `/v1/traces`, `/v1/metrics`, or
  `/v1/logs` appended for HTTP, matching what an OTLP SDK would do
- headers from `OTEL_EXPORTER_OTLP_HEADERS` and the per-signal headers,
  percent-decoded
- `log_user_prompt = true` when `trg` content capture is on, in any mode

This only works under `--environment isolated`, the default, because that is
the only policy where the run owns its config home. Under `scrubbed` or
`inherited`, writing the table would edit your own Codex config, so `trg`
skips it and logs a warning to `trg.log`. Nothing is written when no endpoint
is configured.

`trg` keeps rebuilding `chat` and `execute_tool` spans for Codex runs even
when Codex exports its own, so expect both in the trace.

The run's trace context still reaches Codex through `TRACEPARENT` in either
case.

### `cursor-agent`

Nothing is forwarded. `cursor-agent` has no way to take exporter settings, so
`trg` rebuilds spans from its output as it does without the flag.

## What reaches MCP mock servers

Mock servers are child processes of the harness, started from the MCP config
`trg` writes for the run. What that config gives each mock server depends on
the flag:

| Setting | Without `--forward-telemetry` | With `--forward-telemetry` |
| --- | --- | --- |
| `TRACEPARENT`, `TRACESTATE`, `BAGGAGE` | Yes, whenever a trace is active | Yes, whenever a trace is active |
| `OTEL_*` exporter settings | No | Yes |
| `OTEL_EXPORTER_OTLP_*HEADERS` | No | Yes, in the launch copy only |

Trace context lets the mock's operations join the run's trace. Without the
flag the mock server has no exporter settings, so it exports nothing of its
own.

`trg` writes the MCP config twice. The recorded copy, `mcp-config.json` and
`mcp-config.toml` in the run directory, goes into the report bundle and never
contains header variables.
The launch copy, under `mcp-launch/` in the run directory, is the one handed
to the harness. It carries the header variables so a mock server can
authenticate to your collector, and it is written owner-only (mode `0600`).

## Keep credentials out of the bundle

`env.json` in each run directory records what the harness received. Exporter
header variables (`OTEL_EXPORTER_OTLP_*HEADERS`) are never written there, and
neither are other secret-looking values. The same holds for the recorded MCP
config. Rendered headers live only in files written with mode `0600`: the
Codex `config.toml` in the run-owned config home, and the MCP config under
`mcp-launch/`. Leave `mcp-launch/` out of anything you share or
archive.

## Related

- [Run environment](../reference/ai-skills-eval.md#run-environment)
- [Telemetry reference](../reference/telemetry.md#trace-context-propagation)
- [Telemetry design](../explanation/telemetry-design.md)
