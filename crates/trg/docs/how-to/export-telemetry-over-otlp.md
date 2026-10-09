# Export telemetry over OTLP

Send `trg` traces, metrics, and logs to an OpenTelemetry Collector or any
backend that accepts OTLP over HTTP. Export is off until you configure an
endpoint, and every setting comes from the standard `OTEL_*` environment
variables. `trg` has no telemetry section in its config file.

For the full list of what gets exported, see the
[telemetry reference](../reference/telemetry.md).

## Before you start

- `trg` exports over **http/protobuf only**. Point it at the collector's OTLP
  HTTP receiver (port `4318` by convention), not the gRPC one (`4317`).
- Telemetry never writes to stdout. A setting that keeps a signal from
  exporting, such as `grpc` or an unsupported exporter, is reported as a
  single line on stderr when `trg` starts. Every other problem, such as an
  unreachable collector, goes only to the local file log, so keep it at hand
  while setting up:

  ```sh
  tail -f "${XDG_CACHE_HOME:-$HOME/.cache}/trg/trg.log"
  ```

## Run a local collector

Save this as `otel-collector.yaml`. It accepts OTLP over HTTP and prints
everything it receives:

```yaml
receivers:
  otlp:
    protocols:
      http:
        endpoint: 0.0.0.0:4318

exporters:
  debug:
    verbosity: detailed

service:
  pipelines:
    traces:
      receivers: [otlp]
      exporters: [debug]
    metrics:
      receivers: [otlp]
      exporters: [debug]
    logs:
      receivers: [otlp]
      exporters: [debug]
```

Start it with the upstream collector image:

```sh
docker run --rm -p 4318:4318 \
  -v "$PWD/otel-collector.yaml:/etc/otelcol/config.yaml" \
  otel/opentelemetry-collector:latest
```

The contrib distribution works the same way. Its image is
`otel/opentelemetry-collector-contrib` and it reads its config from
`/etc/otelcol-contrib/config.yaml`.

## Turn export on

Set one endpoint for every signal:

```sh
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
trg doctor
```

The collector prints a trace whose root span is named `trg`, carrying
`trg.command = "doctor"` on its resource, followed by metrics and log records.
`trg` waits up to three seconds on exit to flush what it buffered, so a short
command still delivers its telemetry.

The shared endpoint is a base URL. The exporter appends `/v1/traces`,
`/v1/metrics`, and `/v1/logs` to it.

## Send each signal somewhere else

A per-signal endpoint is used exactly as written, with nothing appended, and
wins over the shared one for that signal:

```sh
export OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=https://traces.example.com/v1/traces
export OTEL_EXPORTER_OTLP_METRICS_ENDPOINT=https://metrics.example.com/v1/metrics
export OTEL_EXPORTER_OTLP_LOGS_ENDPOINT=https://logs.example.com/v1/logs
```

A signal is exported when it has an endpoint of its own or the shared
endpoint is set. Setting only `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` exports
traces and nothing else.

## Turn one signal off

Set that signal's exporter to `none`:

```sh
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
export OTEL_METRICS_EXPORTER=none
export OTEL_LOGS_EXPORTER=none
```

`OTEL_TRACES_EXPORTER`, `OTEL_METRICS_EXPORTER`, and `OTEL_LOGS_EXPORTER`
accept a comma-separated list, compared without regard to case:

| Value | Result |
| ----- | ------ |
| unset, empty, or `otlp` | Exported over OTLP when an endpoint is set |
| `none` | Not exported |
| anything else, such as `console` | Not exported, and reported on stderr |

`trg` ships only an OTLP exporter, and `console` would write to the stdout
that `trg mcp proxy` needs for JSON-RPC. Rather than export something you did
not ask for, it turns the signal off and says so, even when no endpoint is
set:

```text
trg: not exporting traces telemetry: OTEL_TRACES_EXPORTER=console unsupported; trg exports OTLP over http/protobuf only
```

To turn everything off while leaving the endpoints in place, set
`OTEL_SDK_DISABLED=true`.

## Authenticate to the collector

Pass headers the standard way, as comma-separated `key=value` pairs:

```sh
export OTEL_EXPORTER_OTLP_HEADERS="authorization=Bearer%20${COLLECTOR_TOKEN}"
```

Use `OTEL_EXPORTER_OTLP_TRACES_HEADERS`, `OTEL_EXPORTER_OTLP_METRICS_HEADERS`,
or `OTEL_EXPORTER_OTLP_LOGS_HEADERS` for headers that only one signal needs.
Values are percent-decoded, so encode a space as `%20`.

These headers are never written into a run's recorded environment or into the
MCP config kept in a report bundle. See
[Forward telemetry into eval harnesses](forward-telemetry-to-eval-harnesses.md).

## If your collector only speaks gRPC

`trg` does not export over gRPC. When `OTEL_EXPORTER_OTLP_PROTOCOL=grpc`, or
the per-signal `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL` (and its metrics and logs
siblings) is `grpc`, that signal is **not exported at all**. The command still
runs, and prints one line on stderr naming every signal it skipped and the
setting responsible:

```text
trg: not exporting traces, metrics, logs telemetry: OTEL_EXPORTER_OTLP_PROTOCOL=grpc unsupported; trg exports OTLP over http/protobuf only
```

The per-signal protocol variable wins over the shared one, so you can keep
`grpc` for other tools and set, for example,
`OTEL_EXPORTER_OTLP_TRACES_PROTOCOL=http/protobuf` for `trg`. Otherwise enable
the OTLP HTTP receiver on the collector as shown above.

## Name the service and sample traces

The resource defaults to `service.name = "trg"`. The usual SDK variables
apply on top:

```sh
export OTEL_SERVICE_NAME=trg-ci
export OTEL_RESOURCE_ATTRIBUTES=deployment.environment.name=ci,vcs.repository.name=skills
```

Sampling follows `OTEL_TRACES_SAMPLER` and `OTEL_TRACES_SAMPLER_ARG`. The
default, `parentbased_always_on`, keeps every trace and respects the decision
of a parent passed in through `TRACEPARENT`.

## Nest `trg` under a trace you already have

When `TRACEPARENT` (and optionally `TRACESTATE` and `BAGGAGE`) is set in the
environment and traces are on, the `trg` root span becomes a child of that
context. A CI job or a wrapper script that exports its own trace can hand it
to `trg` this way:

```sh
export TRACEPARENT=00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01
trg doctor
```

## Check that it worked

- No trace arrives: check stderr for a `trg: not exporting` line, look in
  `trg.log` for an exporter build failure, and confirm the endpoint is
  reachable from the machine running `trg`.
- Only some signals arrive: check for a per-signal `*_EXPORTER` other than
  `otlp`, a per-signal `*_PROTOCOL=grpc`, or a per-signal endpoint that overrides the
  shared one.
- The file log is too noisy or too quiet: `RUST_LOG` tunes the file log only.
  It never changes what is exported.

## Related

- [Telemetry reference](../reference/telemetry.md)
- [Capture GenAI message content](capture-genai-message-content.md)
- [Forward telemetry into eval harnesses](forward-telemetry-to-eval-harnesses.md)
- [Replay a report bundle into a telemetry backend](replay-a-report-bundle.md)
- [Telemetry design](../explanation/telemetry-design.md)
