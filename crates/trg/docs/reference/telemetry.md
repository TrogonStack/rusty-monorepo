# Telemetry reference

Everything `trg` exports over OpenTelemetry: the variables that control it, the
resource, the spans each command emits, metrics, log events, trace context
propagation, and the `trg.*` attributes. For setup steps, see
[Export telemetry over OTLP](../how-to/export-telemetry-over-otlp.md).

Names follow the OpenTelemetry semantic conventions for GenAI, MCP, HTTP, RPC,
process, and CLI where one applies, and the `trg.*` namespace where none does.
Generated GenAI and MCP names are pinned to semantic-conventions-genai commit
`06ec68e722c45a7218e23ea1bc1339fe4e21ecae`. Many of these conventions are still
in development upstream, so names may change when that pin moves.

## Environment variables

`trg` has no telemetry config file section. Every setting is an environment
variable.

| Variable | Effect |
| -------- | ------ |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Base URL for every signal. `/v1/traces`, `/v1/metrics`, `/v1/logs` are appended. Setting it turns on every signal |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, `..._METRICS_ENDPOINT`, `..._LOGS_ENDPOINT` | Full URL for one signal, used as given. Setting one turns on that signal |
| `OTEL_TRACES_EXPORTER`, `OTEL_METRICS_EXPORTER`, `OTEL_LOGS_EXPORTER` | Unset, empty, or `otlp` exports that signal. `none` turns it off. Any other value, including a list that mixes `otlp` with another exporter, turns it off and is reported on stderr. Values are comma-separated and compared case-insensitively |
| `OTEL_SDK_DISABLED` | `true` (any case) turns every signal off |
| `OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_EXPORTER_OTLP_{TRACES,METRICS,LOGS}_PROTOCOL` | `grpc` turns that signal off and is reported on stderr. The per-signal variable wins. Any other value exports over http/protobuf |
| `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_EXPORTER_OTLP_{TRACES,METRICS,LOGS}_HEADERS` | Request headers, comma-separated `key=value`, percent-decoded |
| `OTEL_SERVICE_NAME` | Overrides `service.name` |
| `OTEL_RESOURCE_ATTRIBUTES` | Merged into the resource |
| `OTEL_TRACES_SAMPLER`, `OTEL_TRACES_SAMPLER_ARG` | Trace sampler. Default `parentbased_always_on` |
| `OTEL_SEMCONV_STABILITY_OPT_IN`, `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT` | Content capture. See [Content capture](#content-capture) |
| `TRACEPARENT`, `TRACESTATE`, `BAGGAGE` | Parent context for the root span. See [Trace context propagation](#trace-context-propagation) |
| `RUST_LOG` | Filter for the local file log only. Never affects export |

A signal is exported when it has an endpoint, its exporter is unset, empty, or
`otlp`, its protocol is not `grpc`, and the SDK is not disabled. If building an
exporter fails, that signal is skipped with a warning in the file log.

### Misconfiguration on stderr

An unsupported exporter value or a `grpc` protocol is reported once per
process, as a single stderr line that names every signal it turned off and
every setting responsible:

```text
trg: not exporting traces, logs telemetry: OTEL_TRACES_EXPORTER=console, OTEL_EXPORTER_OTLP_LOGS_PROTOCOL=grpc unsupported; trg exports OTLP over http/protobuf only
```

An unsupported exporter value is reported even when no endpoint is set. The
same problems are also written to the file log as warnings.

### Pipeline

| Signal | Processor | Scope |
| ------ | --------- | ----- |
| Traces | Batch span processor | Spans from `trg` at `INFO` and above |
| Metrics | Periodic reader | Every instrument listed under [Metrics](#metrics) |
| Logs | Batch log processor | Events from `trg` at `INFO` and above, other crates at `WARN` and above. Events from the HTTP, TLS, gRPC, and OpenTelemetry crates are never exported |

On exit, `trg` shuts the providers down and waits up to 3 seconds for them to
flush. On `SIGINT`, `SIGTERM`, or `SIGHUP`, it ends the root span, waits up to
2 seconds to flush, and then re-raises the signal. Before replacing itself with
`exec(2)` (as `trg exec` does), it ends the root span and flushes.

Nothing telemetry-related is ever written to stdout, which `trg mcp proxy`
uses for JSON-RPC. Apart from the misconfiguration line above, exporter errors
go to the file log only.

### Local file log

Written on every invocation, whether or not export is on, to
`$XDG_CACHE_HOME/trg/trg.log`, or `$HOME/.cache/trg/trg.log` when
`XDG_CACHE_HOME` is unset. Appended to, never rotated. The default filter is
`info,trg=debug,rmcp=debug`, and `RUST_LOG` replaces it. An invalid `RUST_LOG`
falls back to the default.

## Resource

| Attribute | Value |
| --------- | ----- |
| `service.name` | `trg`, unless `OTEL_SERVICE_NAME` is set |
| `service.version` | The `trg` crate version |
| `service.instance.id` | A random 32-character hex id, unique per process |
| `trg.command` | The subcommand path, for example `doctor`, `exec list`, `mcp proxy`, `ai skills eval run` |

`OTEL_RESOURCE_ATTRIBUTES` is merged on top.

## Root span

Every traced invocation has one root span named `trg`.

| Attribute | Value |
| --------- | ----- |
| `process.executable.name` | `trg` |
| `process.pid` | The process id |
| `process.exit.code` | The exit code, or `128 + signal` when interrupted (`130` for `SIGINT`, `143` for `SIGTERM`, `129` for `SIGHUP`) |
| `error.type` | `nonzero_exit` for a non-zero exit, `SIGINT`, `SIGTERM`, or `SIGHUP` when interrupted |

An interrupted root span also has status `Error` with the description
`interrupted by <signal>`. Command arguments are never recorded, since they can
carry secrets. A root span ended before `exec(2)` carries no exit code.

## Spans by command

Span names below are the exported names. `{x}` marks a part filled in at run
time. Unless noted, spans are `INTERNAL` and set `error.type` when the step
fails.

### Child processes

Child processes `trg` runs and waits for, other than harnesses, are `CLIENT`
spans named after the executable: `security`, `op`, `sh`, grader scripts,
harness `--version` probes, scaffold scripts, and the real server behind
`eval record-mcp`. Harnesses get an [`invoke_agent`](#trg-ai-skills-eval-run)
span instead.

| Attribute | Value |
| --------- | ----- |
| `process.executable.name` | The executable name |
| `process.pid` | The child's process id |
| `process.exit.code` | The child's exit code |
| `error.type` | `nonzero_exit`, `signaled`, or `timeout` |

### Config and secrets

Shared by every command that loads config or resolves secrets.

| Span | Attributes |
| ---- | ---------- |
| `load config` | `error.type` |
| `fetch secrets` | `trg.secrets.var.count` |
| `fetch secret` | `trg.secrets.backend.name`, `trg.secrets.backend.kind`, `trg.secrets.var.count` |
| `security`, `op` (CLIENT) | The child process attributes, plus `trg.secrets.backend.kind` and `trg.secrets.operation` |
| `{method} {url.template}` (CLIENT, OpenBao) | `http.request.method`, `url.template`, `server.address`, `server.port`, `http.response.status_code`, `trg.secrets.backend.kind`, `error.type` (the status code when `>= 400`) |

OpenBao URL templates are `/v1/sys/health`, `/v1/{mount}/data/{path}`, and
`/v1/{mount}/metadata/{path}`. A non-standard HTTP method is recorded as
`_OTHER` with `http.request.method_original`, and the span name starts with
`HTTP`.

### `trg exec`

`merge env` with `trg.exec.env.var.count`, alongside the config and secrets
spans.

### `trg secret`

`secret get` and `secret put` with `trg.secrets.backend.name`,
`trg.secrets.backend.kind`, and `error.type`. `read stdin` when the value is
read from standard input.

### `trg doctor`

`diagnose backend` per backend, with `trg.secrets.backend.name`,
`trg.secrets.backend.kind`, and `trg.doctor.healthy`.

### `trg mcp auth` and OAuth

| Span | Attributes |
| ---- | ---------- |
| `ensure credentials` | `trg.mcp.server.name`, `trg.mcp.auth.outcome`, `error.type` |
| `discover oauth_metadata`, `refresh access_token`, `authorize mcp_server`, `register oauth_client`, `exchange oauth_code` | `trg.mcp.server.name`, `error.type` |
| `wait oauth_callback` | `trg.oauth.callback.outcome` |
| `load credentials`, `save credentials`, `clear credentials`, `check shared_credentials` | `error.type` |
| `{method}` (CLIENT, OAuth HTTP) | `http.request.method`, `server.address`, `server.port`, `url.full` (without query), `http.response.status_code`, `error.type` (status code or `transport_error`) |

`error.type` values on auth spans: `authorization_required`,
`authorization_failed`, `token_exchange_failed`, `token_refresh_failed`,
`token_refresh_rejected`, `credential_store_error`, `http_error`,
`oauth_error`, `metadata_error`, `pkce_unsupported`, `url_error`,
`no_authorization_support`, `internal_error`, `invalid_token_type`,
`token_expired`, `invalid_scope`, `registration_failed`, `insufficient_scope`,
`authorization_server_mismatch`, `authorization_server_missing_issuer`,
`client_credentials_error`, `auth_error`.

### `trg mcp proxy`

One `mcp session` span covers the whole session.

| Attribute | Value |
| --------- | ----- |
| `trg.mcp.server.name` | The `[mcp.servers.<name>]` entry |
| `trg.mcp.auth.outcome` | How credentials were obtained |
| `trg.mcp.exit.reason` | `host_eof`, `remote_closed`, or `local_closed` |
| `mcp.protocol.version` | From the server's `initialize` response |
| `mcp.session.id` | The remote session id |
| `network.transport`, `network.protocol.name` | `tcp`, `http` |
| `server.address`, `server.port` | The remote server |
| `error.type` | Unset for `host_eof`. Otherwise the exit reason or the failure |

Notifications are span events named `mcp notification` on the session span,
with `mcp.method.name` and `trg.mcp.message.direction`.

Each request is a `CLIENT` span named `{mcp.method.name} {target}`, where the
target is the tool name for `tools/call` and the prompt name for
`prompts/get`, or just `{mcp.method.name}` otherwise.

| Attribute | Value |
| --------- | ----- |
| `mcp.method.name` | The JSON-RPC method |
| `jsonrpc.request.id` | The request id |
| `gen_ai.operation.name` | `execute_tool` for `tools/call` |
| `gen_ai.tool.name` | For `tools/call` |
| `gen_ai.prompt.name` | For `prompts/get` |
| `mcp.resource.uri` | For `resources/read`, `resources/subscribe`, `resources/unsubscribe` |
| `gen_ai.tool.call.arguments`, `gen_ai.tool.call.result` | Content capture only |
| `rpc.response.status_code` | The JSON-RPC error code, when there is one |
| `error.type` | The JSON-RPC error code, `tool_error` when the tool result has `isError`, or the session exit reason for a request still pending at exit |
| `mcp.session.id`, `mcp.protocol.version`, `network.*`, `server.*` | As on the session |

When the host's request carries `_meta.traceparent`, the request span is a
child of that context and links to the session span. Otherwise it is a child of
the session span. With traces on, the proxy rewrites `_meta.traceparent` to its
own request span before forwarding. Requests refused over stdio are recorded as
error spans, and their `mcp.client.operation.duration` carries the refusal's
JSON-RPC error code as `error.type`.

### `trg ai skills eval run`

The suite span is `invoke_workflow {skill name}`.

| Attribute | Value |
| --------- | ----- |
| `gen_ai.operation.name` | `invoke_workflow` |
| `gen_ai.workflow.name` | The skill name |
| `trg.eval.skill.revision`, `trg.eval.iteration`, `trg.eval.scenarios`, `trg.eval.grading.strategy`, `trg.eval.concurrency` | See [Attribute registry](#attribute-registry) |
| `error.type` | Set when the pass fails |

Suite steps, as children of the suite span: `validate skill`, `check suite`,
`lint fixtures`, `detect iteration`, `resolve scenarios`,
`check runner availability` (with a child process span per `--version` probe),
`open cost ledger`, `admit skill` (with `await trust answer`, carrying
`trg.eval.trust.answer`), `build report bundle`, `write report bundle`,
`load run inputs`, `execute runs` (with `trg.eval.concurrency` and
`trg.eval.run.count`), `hash skills`, `check skill tampering`,
`rewrite report`, `finish suite`, `collect report metrics`, `check ci gates`,
`emit github annotations`.

Each run is a `run` span under `execute runs`.

| Attribute | Value |
| --------- | ----- |
| `trg.eval.run.id`, `trg.eval.case.id`, `trg.eval.case.split`, `trg.eval.scenario`, `trg.eval.iteration` | Which cell of the suite this is |
| `trg.eval.model_config`, `trg.eval.runner.kind`, `trg.eval.runner.version`, `trg.eval.runner.model`, `trg.eval.tool_grant` | What it ran on |
| `trg.eval.lane` | The `-j` lane |
| `trg.eval.cache.hit` | Whether it was served from cache |
| `trg.eval.run.status` | The run's final status |
| `error.type` | Set when the run fails |

Run steps: `resolve mocks`, `hash fixtures`, `compute cache key`,
`look up cache`, `restore from cache`, `admit budget`, `materialize mocks`,
`hash skill`, `apply outcome`, `record cache entry`, `check skill tampering`.

Each harness invocation is an `attempt` span with `trg.eval.attempt`,
`trg.eval.attempt.outcome`, `trg.eval.attempt.transient`, and `error.type`.
Its children:

- `verify read-only fixtures`
- `prepare workspace`, with `reset workspace`, `stage skill`,
  `stage companions`, `stage fixtures`, `build prompt`, `prepare environment`,
  and a child process span per scaffold script
- `write runner invocation`, `declare mock servers` (codex), `declare otel`
  (codex), `parse outcome`, `write outcome`
- `persist runner io`, with `redact transcript`, `write transcript`,
  `write stderr`, `normalize transcript`
- the harness span below

The harness span is a `CLIENT` span named `invoke_agent {agent}`, where the
agent is `claude-code`, `codex`, or `cursor-agent`.

| Attribute | Value |
| --------- | ----- |
| `gen_ai.operation.name` | `invoke_agent` |
| `gen_ai.provider.name` | `anthropic`, `openai`, or `cursor` |
| `gen_ai.agent.name` | The agent name |
| `gen_ai.request.model` | The model `trg` asked the harness for. Absent when no model was requested |
| `gen_ai.response.model` | The model the harness reported |
| `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `gen_ai.usage.cache_read.input_tokens`, `gen_ai.usage.cache_write.input_tokens` | As reported by the harness |
| `trg.eval.cost.usd` | As reported by the harness |
| `trg.eval.harness.duration_ms`, `trg.eval.harness.api_duration_ms` | As reported by the harness |
| `process.executable.name` | `claude`, `codex`, or `cursor-agent` |
| `process.pid`, `process.exit.code` | The harness process |
| `error.type` | `nonzero_exit`, `signaled`, `timeout`, `spawn_failed`, `io`, `invalid_output`, `unsupported_scenario`, `json`, or `harness_error` |

`trg` rebuilds the harness's steps from its output as children of
`invoke_agent`. The one exception is `claude-code` run with
`--forward-telemetry`, a traces endpoint, and `OTEL_TRACES_EXPORTER` not set
to `none`: Claude Code then exports its own spans and nothing is rebuilt (see
[Forward telemetry into eval harnesses](../how-to/forward-telemetry-to-eval-harnesses.md)).


| Span | Attributes |
| ---- | ---------- |
| `chat {model}` (CLIENT) | `gen_ai.operation.name` (`chat`), `gen_ai.request.model` (only when `trg` requested a model), `gen_ai.response.model`, `gen_ai.response.id`, token usage, `error.type` |
| `execute_tool {tool}` | `gen_ai.operation.name` (`execute_tool`), `gen_ai.tool.name`, `gen_ai.tool.call.id`, `gen_ai.tool.call.arguments` and `gen_ai.tool.call.result` (content capture only), `error.type` |

The `chat` span is named for the requested model, else the reported one, and
is plain `chat` when neither is known. For `codex`, each stretch of a turn in
which no tool is running becomes its own `chat` span. Codex does not name the
model it answered with, so these spans carry only the requested model, and the
turn's token usage lands on its last `chat` span.

`error.type` on rebuilt spans is `tool_error`, `turn_failed`, `timeout`, or
`unfinished`. A tool still running when the harness is killed for exceeding
its timeout ends at the kill time with `timeout`. A tool still running when the
harness exits on its own ends with `unfinished`.

MCP mock servers started by the harness report into the same trace. See
[`trg ai skills eval mock-server`](#trg-ai-skills-eval-mock-server).

### `trg ai skills eval grade`

Also emitted by `eval run --grade`.

| Span | Attributes |
| ---- | ---------- |
| `grade` | |
| `open judge session` | `gen_ai.request.model` |
| `grade run` | `trg.eval.run.id`, `trg.eval.case.id`, `trg.eval.case.split`, `trg.eval.scenario`, `trg.eval.iteration`. Links to the run span recorded in `report.json` |
| `grader {kind}` | `trg.eval.grader.name`, `trg.eval.grader.kind` |
| `write grading results` | |

Script graders add a child process span. LLM graders add a `CLIENT` span per
ballot, named `chat {model}`:

| Attribute | Value |
| --------- | ----- |
| `gen_ai.operation.name` | `chat` |
| `gen_ai.provider.name` | `anthropic` or `openai`. OpenAI-compatible endpoints report `openai` |
| `gen_ai.request.model`, `gen_ai.response.model`, `gen_ai.response.id` | |
| `gen_ai.request.max_tokens` | Anthropic only |
| `gen_ai.output.type` | `json`, OpenAI only |
| `gen_ai.usage.*` | Token usage |
| `server.address`, `server.port` | The provider endpoint |
| `gen_ai.system_instructions`, `gen_ai.input_messages`, `gen_ai.output_messages` | Content capture only |
| `error.type` | Set when the call fails |

Each ballot's HTTP request is a child `CLIENT` span named `POST` with
`http.request.method`, `url.full` (userinfo and query values replaced with
`REDACTED`), `server.address`, `server.port`, `http.response.status_code`, and
`error.type`.

Grading also emits [evaluation events](#evaluation-events).

### `trg ai skills eval compare`

| Span | Attributes |
| ---- | ---------- |
| `compare` | |
| `compare case` | `trg.eval.case.id` |
| `compare pair` | `trg.eval.case.id`, `trg.eval.comparison.pair`. Links to the run spans and the `grade run` spans of both sides, when `report.json` recorded them |
| `load outputs`, `build blind pair`, `write comparison`, `write report` | |

The judge is a `chat {model}` span as in grading, or an `sh` child process span
for `--judge script`.

### `trg ai skills eval export`

Replays a report bundle as a new trace, using the span names a live pass
records and marking every span with `trg.eval.replayed`. See
[`eval export`](ai-skills-eval.md#eval-export) for what a replay contains and
[Replay a report bundle into a telemetry backend](../how-to/replay-a-report-bundle.md)
for when to use it.

### `trg ai skills eval verify`

`validate schemas`, `check workspace`, `collect results`, `run ci checks`,
`emit annotations`, `check skill`, `validate skill`, `check eval suite`,
`load eval suite`, `lint fixtures`.

### `trg ai skills eval benchmark`

`benchmark`, `read report`, `aggregate scenarios`, `check eval suite drift`,
`build headroom`, `write benchmark`.

### `trg ai skills eval grader-agreement`

`grader agreement`, `load report`, `collect verdicts`,
`write grader agreement`.

### `trg ai skills eval record-mcp`

| Span | Attributes |
| ---- | ---------- |
| `admit real server` | |
| `{executable}` (CLIENT) | The real server process |
| `initialize`, `tools/call {tool}` (CLIENT) | `mcp.method.name`, `mcp.protocol.version` (`2024-11-05`), `jsonrpc.request.id`, `network.transport` (`pipe`), `gen_ai.operation.name`, `gen_ai.tool.name`, `gen_ai.tool.call.arguments` and `gen_ai.tool.call.result` (content capture only), `rpc.response.status_code`, `error.type` |
| `write mock` | `gen_ai.tool.name` |
| `finish` | |

`error.type` on request spans: `spawn_failed`, `server_exited`, `timeout`,
`write_failed`, `wait_failed`, `io`, `unrepresentable_placeholder`,
`tool_error`, or the JSON-RPC error code.

### `trg ai skills eval mock-server`

Each request is a `SERVER` span named `{mcp.method.name} {tool}`, with
`mcp.method.name`, `jsonrpc.request.id`, `gen_ai.tool.name`,
`gen_ai.operation.name`, `trg.eval.mock.server`, `trg.eval.mock.match`,
`rpc.response.status_code`, and `error.type`. Each request also records
`mcp.server.operation.duration`.

## Metrics

Durations are histograms in seconds.

| Instrument | Type | Unit | Attributes | Emitted by |
| ---------- | ---- | ---- | ---------- | ---------- |
| `mcp.client.operation.duration` | Histogram | `s` | `mcp.method.name`, `gen_ai.tool.name` or `gen_ai.prompt.name`, `mcp.protocol.version`, `network.transport`, `error.type`; `record-mcp` adds `gen_ai.operation.name` and `rpc.response.status_code` | `mcp proxy` (forwarded and refused requests), `eval record-mcp` |
| `mcp.server.operation.duration` | Histogram | `s` | `mcp.method.name`, `network.transport` (`pipe`), `gen_ai.operation.name` and `gen_ai.tool.name` for tool calls, `rpc.response.status_code`, `error.type` | `eval mock-server` |
| `mcp.client.session.duration` | Histogram | `s` | `mcp.protocol.version`, `network.transport`, `error.type` | `mcp proxy` |
| `gen_ai.invoke_workflow.duration` | Histogram | `s` | `gen_ai.operation.name`, `gen_ai.workflow.name`, `error.type` | `eval run` |
| `gen_ai.invoke_agent.duration` | Histogram | `s` | `gen_ai.operation.name`, `gen_ai.provider.name`, `gen_ai.agent.name`, `gen_ai.request.model`, `error.type` | `eval run` |
| `gen_ai.invoke_agent.tool_calls` | Histogram | | Same as `gen_ai.invoke_agent.duration` | `eval run` |
| `gen_ai.invoke_agent.inference_calls` | Histogram | | Same as `gen_ai.invoke_agent.duration` | `eval run` |
| `gen_ai.execute_tool.duration` | Histogram | `s` | `gen_ai.operation.name`, `gen_ai.provider.name`, `gen_ai.tool.name`, `error.type` | `eval run` |
| `trg.eval.cache.lookups` | Counter | `{lookup}` | `trg.eval.cache.hit` | `eval run` |
| `trg.eval.attempts` | Counter | `{attempt}` | `trg.eval.attempt.outcome` (`completed`, `transient_failure`, `runner_error`) | `eval run` |
| `trg.eval.runs` | Counter | `{run}` | `trg.eval.run.status`, `trg.eval.scenario` | `eval run` |
| `trg.eval.cost` | Counter | `USD` | `trg.eval.run.status`, `trg.eval.scenario`. Not counted for cache hits | `eval run` |
| `gen_ai.client.inference.duration` | Histogram | `s` | `gen_ai.operation.name`, `gen_ai.provider.name`, `gen_ai.request.model`, `gen_ai.response.model`, `server.address`, `server.port`, `error.type` | LLM judges |
| `gen_ai.client.inference.usage.input_tokens`, `.output_tokens`, `.cache_read.input_tokens`, `.cache_write.input_tokens` | Counter | `{token}` | As `gen_ai.client.inference.duration` | LLM judges |
| `gen_ai.client.inference.operation.input_tokens`, `.output_tokens` | Histogram | `{token}` | As `gen_ai.client.inference.duration` | LLM judges |
| `trg.eval.assertions` | Counter | `{assertion}` | `gen_ai.evaluation.score.label`, `trg.eval.grader.kind` | `eval grade` |

`error.type` on `gen_ai.invoke_workflow.duration` is `gate_failed`,
`budget_exhausted`, `infrastructure_failure`, or `interrupted`.

The MCP duration histograms use the bucket boundaries `0.01`, `0.02`, `0.05`,
`0.1`, `0.2`, `0.5`, `1`, `2`, `5`, `10`, `30`, `60`, `120`, `300`.

## Evaluation events

Grading, comparison, and replay emit log records named
`gen_ai.evaluation.result`. They are sent on the logs signal only and are never
attached to spans as span events, whatever the content capture mode. Each
record carries a trace context:

- Grading uses the run span recorded in `report.json`, so the record joins that
  run's trace even though the run finished in an earlier process. Without a
  recorded run span it uses the active `grade run` span.
- Comparison uses the `compare pair` span.
- Replay uses the replayed run span.

| Attribute | Value |
| --------- | ----- |
| `gen_ai.evaluation.name` | See below |
| `gen_ai.evaluation.score.label` | See below |
| `gen_ai.evaluation.score.value` | See below |
| `gen_ai.evaluation.explanation` | The grader's evidence. `EVENT_ONLY` or `SPAN_AND_EVENT` content capture only |
| `trg.eval.grader.kind` | The grader type |
| `trg.eval.case.id` | The case |
| `trg.eval.run.id` | The run. Absent on comparisons |
| `trg.eval.replayed` | `true` on records sent by `eval export`. Absent otherwise |

| Event | `gen_ai.evaluation.name` | Label | Score |
| ----- | ------------------------ | ----- | ----- |
| One per assertion | The grader's declared name, or its kind | `pass`, `fail`, `unsupported`, `excluded`, `ungraded` | Votes passed over votes cast when the grader voted, else `1.0` for pass and `0.0` for fail, else absent |
| Read-only fixture check | `read_only_fixture` | As above | As above |
| Mock expectation check | `mock_expectation` | As above | As above |
| Per-run score | `case_score` | | The run's `case_score`. Not emitted when grading scored nothing for the run |
| Comparison | `comparison` | The winning scenario, or `tie` | `1.0` when side a wins, `0.0` when side b wins, `0.5` for a tie |

Comparisons set `trg.eval.grader.kind` to `llm` or `script`. The per-run score
carries no `trg.eval.grader.kind`.

## Content capture

Content is captured only when `OTEL_SEMCONV_STABILITY_OPT_IN` contains
`gen_ai_latest_experimental` and
`OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT` is exactly one of these
values:

| Value | Span content | Event content |
| ----- | ------------ | ------------- |
| `SPAN_ONLY` | yes | no |
| `EVENT_ONLY` | no | yes |
| `SPAN_AND_EVENT` | yes | yes |
| anything else, or unset | no | no |

Span content is `gen_ai.tool.call.arguments`, `gen_ai.tool.call.result`,
`gen_ai.system_instructions`, `gen_ai.input_messages`, and
`gen_ai.output_messages`. Event content is `gen_ai.evaluation.explanation`.
Any mode also changes what is forwarded to harnesses; see
[Capture GenAI message content](../how-to/capture-genai-message-content.md).

## Trace context propagation

When a trace exporter is active, `trg` reads `TRACEPARENT`, `TRACESTATE`, and
`BAGGAGE` from its environment as the parent of its root span, and sets them on
the child processes it starts, pointing at the span that started each one:

- harness processes, harness `--version` probes, and scaffold scripts
- script graders and the `sh` script judge in `compare`
- `security` and `op` secrets backend CLIs
- the target of `trg exec`, including after its environment is cleared
- MCP mock servers, through the `env` block of the MCP config written for the
  run. With `--forward-telemetry` that block also carries the `OTEL_*`
  exporter settings; see
  [What reaches MCP mock servers](../how-to/forward-telemetry-to-eval-harnesses.md#what-reaches-mcp-mock-servers)
- the real server driven by `eval record-mcp`

`trg mcp proxy` reads and rewrites `_meta.traceparent` on MCP requests instead.

When traces are off, none of these variables is set, and inherited values pass
through only where the child's environment already allows it.

## Trace references in reports

`report.json` records spans so later commands can link to them:

| Field | Span |
| ----- | ---- |
| `report.trace` | The suite span of the pass that produced the report |
| `runs[].trace` | The `run` span |
| `runs[].grade_trace` | The `grade run` span of the latest grading |

Each is an object with `trace_id` (32 lowercase hex characters) and `span_id`
(16 lowercase hex characters). A reference is written only for a span that was
sampled, so it never points at a span the backend did not receive. `eval grade`
links to `runs[].trace` and emits its evaluation events in that context.
`eval compare` links to both fields of each side. `eval export` uses them to
tell what was already traced live.

`report.telemetry_forwarding` is `"on"` when the pass ran with
`--forward-telemetry`, and absent otherwise.

## Attribute registry

Attributes in the `trg.*` namespace.

| Attribute | Type | Description |
| --------- | ---- | ----------- |
| `trg.command` | string | Resource. The subcommand path |
| `trg.doctor.healthy` | boolean | Every check of a backend diagnosis passed |
| `trg.exec.env.var.count` | int | Variables in the launched command's environment |
| `trg.mcp.server.name` | string | The `[mcp.servers.<name>]` entry |
| `trg.mcp.auth.outcome` | string | `none`, `already_authorized`, `authorized` |
| `trg.mcp.exit.reason` | string | `host_eof`, `remote_closed`, `local_closed` |
| `trg.mcp.message.direction` | string | `host_to_remote`, `remote_to_host` |
| `trg.oauth.callback.outcome` | string | `received`, `timeout`, `state_mismatch`, `provider_error` |
| `trg.secrets.backend.kind` | string | `keychain`, `openbao`, `onepassword` |
| `trg.secrets.backend.name` | string | The `[secrets.backends.<name>]` entry. Never a path or a value |
| `trg.secrets.operation` | string | The fixed backend CLI subcommand, never its arguments |
| `trg.secrets.var.count` | int | Config vars one fetch served |
| `trg.eval.skill.revision` | string | Content hash of the skill revision |
| `trg.eval.scenarios` | string | Scenarios covered, comma-separated in suite order |
| `trg.eval.grading.strategy` | string | `none`, `auto` |
| `trg.eval.concurrency` | int | `-j` |
| `trg.eval.run.count` | int | Runs scheduled |
| `trg.eval.iteration` | int | Iteration number |
| `trg.eval.run.id` | string | Run id, for example `run-001` |
| `trg.eval.case.id` | string | Eval case id |
| `trg.eval.case.split` | string | `train`, `test`, or the case's split |
| `trg.eval.scenario` | string | `with_skill`, `without_skill`, `old_skill` |
| `trg.eval.model_config` | string | `--model-config` label |
| `trg.eval.runner.kind` | string | `claude`, `codex`, `cursor-agent` |
| `trg.eval.runner.version` | string | Harness version from the availability probe |
| `trg.eval.runner.model` | string | Model the run resolved to |
| `trg.eval.tool_grant` | string | Tool grant the run executed under |
| `trg.eval.lane` | int | `-j` lane |
| `trg.eval.cache.hit` | boolean | Run served from cache |
| `trg.eval.cache.restored.files` | int | Files a cache hit restored |
| `trg.eval.cache.restored.bytes` | int | Bytes a cache hit restored |
| `trg.eval.run.status` | string | The run's final status |
| `trg.eval.attempt` | int | Attempt number within a run |
| `trg.eval.attempt.outcome` | string | `completed`, `transient_failure`, `runner_error` |
| `trg.eval.attempt.transient` | boolean | A retry may recover from this failure |
| `trg.eval.cost.usd` | double | Cost the harness reported |
| `trg.eval.harness.duration_ms` | int | Duration the harness reported |
| `trg.eval.harness.api_duration_ms` | int | Time the harness reported waiting on the model API |
| `trg.eval.skill.tampered` | boolean | The skill directory changed during the pass |
| `trg.eval.trust.answer` | string | `yes`, `no`, `unanswered` |
| `trg.eval.mock.server` | string | The mocked MCP server name |
| `trg.eval.mock.match` | string | `matched`, `violated`, `unresolved`, `unknown_tool` |
| `trg.eval.grader.name` | string | Declared grader name |
| `trg.eval.grader.kind` | string | Grader type, for example `contains`, `llm`, `script` |
| `trg.eval.comparison.pair` | string | `{a}:{b}` |
| `trg.eval.replayed` | boolean | The span or event was rebuilt from a report bundle by `eval export`. Absent on live telemetry |
| `trg.eval.report.id` | string | The report id. Set on the replayed suite span |
