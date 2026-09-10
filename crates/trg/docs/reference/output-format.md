# `--output-format` reference

Every `trg` command that produces a result accepts `--output-format`, with the
same two values and the same default everywhere.

| Value | Meaning |
| ----- | ------- |
| `text` | The default. A summary written for a person to read. |
| `json` | A single JSON document written for a program to parse. |

## Where the output goes

Under either format the result goes to **stdout** and anything that stopped the
command from producing one goes to **stderr**. So `--output-format json` piped
into a parser never has prose in front of it:

```console
$ trg mcp auth status --server example --output-format json | jq .client_id
"9d1f0c7e-4a2b-4f10-9c33-6b8e2a5d7f41"
```

A negative verdict is still a result. `trg ai skills validate` with
`--output-format json` on an invalid skill prints the document describing why it
is invalid, and exits non-zero; it does not move the reason to stderr.

Exit codes do not change with the format. The one exception is a document that
cannot be rendered, which exits `1` because the command produced nothing a
caller can read.

## The one command without it

`trg mcp proxy` has no `--output-format`. Its stdout is the JSON-RPC stream it
speaks to whichever client launched it, so there is no result to render and no
choice to offer.

## Secrets

`trg secret get` prints the secret it was asked for, under either format. Under
`json` the value is a field in the document rather than the whole of stdout,
which is a difference in shape and not in exposure. The document also echoes
the address it was given: `path` and `key`, or `ref`, whichever the backend is
addressed by, never both.

`trg mcp auth status` never prints a token under either format. The `json` form
carries the same redacted summary the text form describes.
