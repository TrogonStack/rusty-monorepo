# `trg` config file reference

`trg` reads a single TOML file when it needs configuration (currently only
`trg mcp proxy`). This page is the authoritative description of every
recognised field, its accepted shape, and how values are resolved at load
time.

## Location

`trg` resolves the config path in this order:

1. `$XDG_CONFIG_HOME/trg/config.toml`
2. `$HOME/.config/trg/config.toml`
3. `/.config/trg/config.toml`

The file is read on demand. If it is missing, commands that require it fail
with a clear `config file not found at <path>` error.

## File layout

```toml
[mcp.servers.<name>]
url = "<value>"

[mcp.servers.<name>.vars]
<var-name> = "<literal>"           # or { env = "...", default = "..." }

[mcp.servers.<name>.headers]
<HeaderName> = "<value>"
```

- The top level only recognises `[mcp]`. Unknown top-level keys are rejected.
- `[mcp.servers]` must contain at least one entry — an empty or missing
  servers table fails with `no [mcp.servers] section in config`.
- Each `<name>` becomes the value passed to `trg mcp proxy --server <name>`.
- Unknown fields inside a server entry are rejected (`deny_unknown_fields`).

## Variables (`[mcp.servers.<name>.vars]`)

Every value that needs to come from the environment is declared once in the
server's `vars` table. The rest of the config refers to it by name. This is
the single place env-backed inputs live for a server.

Each entry is one of:

- A literal TOML string:

    ```toml
    api = "v1"
    ```

- An inline env table (`VarSource`):

    ```toml
    host  = { env = "MCP_HOST" }
    token = { env = "MCP_TOKEN", default = "dev-token" }
    ```

  `default` is used when the env var is unset. Without it, an unset env
  variable fails loading with `environment variable <NAME> is required but
  unset`.

Unknown keys in an env table (e.g. `{ env = "X", typo = true }`) are
rejected at parse time.

`vars` is optional — omit it if your server uses only literal values.

## `[mcp.servers.<name>]`

| Field   | Type            | Required | Notes                                                                 |
| ------- | --------------- | -------- | --------------------------------------------------------------------- |
| `url`   | `VarTemplate`   | yes      | Remote MCP HTTP endpoint. Resolved value must not be empty or blank.  |
| `headers` | table of `HeaderName -> VarTemplate` | no | Sent on every request to the remote endpoint. |
| `vars`  | table of `VarSource` | no  | Per-server variable bindings; see above.                              |

### Reserved fields

The following fields are accepted by the parser but are not yet wired into
the proxy runtime. They exist so future releases can light up the behaviour
without breaking existing config files:

- `transport`
- `max_disconnected_time`
- `initial_retry_interval`
- `override_protocol_version`

## `VarTemplate` (used for `url` and header values)

`url` and every header value accept three shapes:

### 1. Literal string

```toml
url = "https://mcp.example.com/v1"
Authorization = "Bearer dev-token"
```

The string is used verbatim. No placeholder or interpolation syntax —
substitution only happens via `vars` and `{ var = "name" }` references.

### 2. Variable reference

```toml
url = { var = "endpoint" }
Authorization = { var = "auth" }
```

The named variable must exist in this server's `vars` table or loading
fails with `undefined variable <name> referenced`.

### 3. Array of segments

```toml
url = [
    "https://",
    { var = "host" },
    "/v1/stream",
]

Authorization = [
    "Bearer ",
    { var = "token" },
]
```

Each array entry is a literal string or a `{ var = "name" }` reference. They
are resolved independently and concatenated in order. An empty array
resolves to the empty string (rejected for `url`, see *Validation*).

### Not accepted in `url` / headers

Inline `{ env = "..." }` tables are **not** allowed directly in `url` or
header values. Declare the env binding in the server's `vars` table and
reference it by name. This keeps env reads in one place per server.

## Headers

- Header names are validated as HTTP header names. Invalid names fail with
  `invalid header name <NAME>`.
- Header names are case-insensitive after canonicalization; declaring
  e.g. both `Authorization` and `authorization` in the same server is
  rejected with `duplicate header ... after canonicalization`.
- Header values are validated as HTTP header values after resolution.
- Empty or whitespace-only resolved values are rejected.

## Validation

- `url` must resolve to a non-empty, non-whitespace string.
- Header values must be non-empty after resolution.
- Each `vars` entry is resolved before any `url`/header resolution — the
  first missing required env aborts loading.
- Every `{ var = "name" }` reference must have a matching `vars` entry.

## Secret handling

- Resolved `url` and header values are wrapped in `SecretString` and only
  exposed at the point of building the outgoing HTTP request.
- Secrets stay in environment variables and are pulled in through `vars`.
  They never need to live in the config file.

## Examples

### Minimal — literal endpoint, no auth, no vars

```toml
[mcp.servers.local]
url = "http://127.0.0.1:8787/mcp"
```

Use with: `trg mcp proxy --server local`.

### Token from the environment via vars

```toml
[mcp.servers.prod]
url = "https://mcp.example.com/v1"

[mcp.servers.prod.vars]
token = { env = "PROD_MCP_TOKEN", default = "Bearer dev-only" }

[mcp.servers.prod.headers]
Authorization = { var = "token" }
X-Tenant      = "acme"
```

### URL composed from static + env-sourced pieces

```toml
[mcp.servers.composed]
url = [
    "https://",
    { var = "host" },
    "/",
    { var = "api" },
    "/stream",
]

[mcp.servers.composed.vars]
host  = { env = "MCP_HOST" }
api   = "v1"
token = { env = "MCP_TOKEN" }

[mcp.servers.composed.headers]
Authorization = ["Bearer ", { var = "token" }]
```

### Endpoint and token both env-sourced (whole strings)

```toml
[mcp.servers.staging]
url = { var = "endpoint" }

[mcp.servers.staging.vars]
endpoint = { env = "STAGING_MCP_URL" }
token    = { env = "STAGING_MCP_TOKEN" }

[mcp.servers.staging.headers]
Authorization = { var = "token" }
```

If either env var is unset at the time `trg mcp proxy` runs, the command
fails fast with the variable name in the error.

## Error reference

| Error                                             | Meaning                                                         |
| ------------------------------------------------- | --------------------------------------------------------------- |
| `config file not found at <path>`                 | No file at the resolved config path.                            |
| `no [mcp.servers] section in config`              | `[mcp]` missing, or `[mcp.servers]` is empty.                   |
| `unknown MCP server <name> — known: ...`          | `--server` does not match any `[mcp.servers.<name>]` key.       |
| `MCP server url must not be empty`                | Resolved URL is empty or whitespace.                            |
| `invalid header name <name>: ...`                 | Header key is not a valid HTTP header name.                     |
| `could not decode header <name>: ...`             | Resolved header value is empty/whitespace or not a valid value. |
| `duplicate header <name> collides with <existing> after canonicalization` | Two header keys map to the same canonical name (e.g. `Authorization` and `authorization`). |
| `environment variable <NAME> is required but unset` | A `vars` entry's env had no `default` and the env var was missing. |
| `undefined variable <NAME> referenced; declare it in [mcp.servers.<name>.vars]` | `{ var = "..." }` references a name not present in `vars`. |
| TOML parse errors                                 | Unknown fields, malformed TOML, or `{ env = "..." }` used directly in `url`/headers (must go through `vars`). |
