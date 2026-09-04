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
[secrets.backends.<backend-name>]
kind = "keychain"                  # or "openbao"

[mcp.servers.<name>]
url = "<value>"
secrets = "<backend-name>"         # optional

[mcp.servers.<name>.vars]
<var-name> = "<literal>"           # or { env = "...", default = "..." }

[mcp.servers.<name>.headers]
<HeaderName> = "<value>"
```

- The top level recognises `[mcp]` and `[secrets]`. Unknown top-level keys are
  rejected.
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
| `secrets` | string        | no       | Name of a `[secrets.backends.<name>]` entry. Omitted means the macOS Keychain. |

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

## `[secrets.backends.<name>]`

Declares where OAuth credentials are stored. A server addresses exactly one
backend by name through its `secrets` field; `trg` never searches a list of
backends and never falls back to a second one when the first fails.

Declaring a backend costs nothing until a server addresses it, so a machine
that cannot reach the OpenBao instance declared here can still use every
server that names a different backend.

Values in a backend declaration accept `VarSource` (a literal string, or
`{ env = "...", default = "..." }`) but **not** `{ secret = "..." }`: nothing
needed to reach the secret store may itself live in the secret store.

### `kind = "keychain"`

| Field     | Type   | Required | Notes                                                    |
| --------- | ------ | -------- | -------------------------------------------------------- |
| `service` | string | no       | Keychain service attribute. Defaults to `trg MCP Credentials`. |

macOS only. On any other platform, operations against this backend fail with
`the keychain backend is available only on macOS`.

### `kind = "openbao"`

Speaks the KV v2 HTTP API of an [OpenBao](https://openbao.org) instance.

| Field          | Type        | Required | Notes                                                                     |
| -------------- | ----------- | -------- | ------------------------------------------------------------------------- |
| `addr`         | `VarSource` | yes      | Base URL. Must start with `http://` or `https://`.                        |
| `mount`        | string      | yes      | KV v2 mount, e.g. `secret`.                                               |
| `path_prefix`  | string      | yes      | Prefix under the mount. May be empty.                                     |
| `machine_id`   | string      | no       | Defaults to `hostname -s`. Scopes credential paths per machine.           |
| `token_file`   | string      | one of   | Path to the token file. `~` expands against `$HOME`.                      |
| `token`        | `VarSource` | one of   | The token itself, usually `{ env = "BAO_TOKEN" }`.                        |
| `ca_cert_file` | string      | no       | PEM bundle. **Replaces** the OS trust store for this backend.             |
| `timeout_ms`   | integer     | no       | Total request budget. Defaults to `5000`.                                 |

Exactly one of `token_file` or `token` must be declared. Declaring neither or
both fails at load time.

`mount`, `machine_id`, and every segment of `path_prefix` must be non-empty
and match `[A-Za-z0-9._-]`, because each becomes a URL path segment.

There is no option to skip TLS verification. A private CA is configured by
pointing `ca_cert_file` at its certificate.

The token is re-read from `token_file` on every operation, so `bao login` in
any terminal recovers a long-running `trg mcp proxy` without restarting the
editor that spawned it. A token file that any other user can read is refused
with a `chmod 600` message rather than used.

### Credential layout

| Backend    | Where one server's credentials live                             |
| ---------- | ---------------------------------------------------------------- |
| `keychain` | Service = the backend's `service`, account = the server name.     |
| `openbao`  | `<mount>/data/<path_prefix>/mcp/<machine_id>/<server-name>`       |

OpenBao paths are scoped per machine because OAuth refresh tokens are
client-bound and providers that rotate them invalidate the previous one on
use, so two machines sharing a path would repeatedly log each other out.

A server stored in OpenBao must be named with `[A-Za-z0-9._-]`, since the name
becomes a path segment. The Keychain accepts any name.

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

## OAuth

`trg mcp proxy` engages OAuth 2.1 (Authorization Code + PKCE) automatically
when the remote endpoint advertises it via RFC 9728 / RFC 8414 discovery and
no `Authorization` header was supplied in the server's `headers`. No config
field opts in — discovery is the trigger.

If you provide `Authorization` in `headers` (e.g. a long-lived PAT), OAuth
is skipped entirely and the static header is used as-is.

### Credential storage

Tokens are persisted in the backend the server names, or in the macOS Keychain
when it names none. See [`[secrets.backends.<name>]`](#secretsbackendsname)
for the layout each backend uses.

The full `rmcp::StoredCredentials` payload (client id, token response,
granted scopes, issued-at timestamp) is JSON-serialised and stored as a single
entry. There is **no** on-disk fallback — if the backend fails for any reason
other than "no such entry", the command aborts with the underlying error.

### First-run vs subsequent runs

- First invocation against a new OAuth server opens the system browser at
  the provider's authorization URL and waits up to 5 minutes for the
  callback on `http://127.0.0.1:<random-port>/oauth/callback`. The
  authorization URL is also echoed on stderr so it can be pasted manually
  if `open(1)` cannot launch a browser.
- Subsequent invocations read the token from the Keychain; the browser is
  not opened. Expired access tokens refresh transparently via the refresh
  token.

### Clearing credentials

Either:

```sh
trg mcp auth logout --server <name>
```

or, for a Keychain-backed server, directly via `security(1)`:

```sh
security delete-generic-password -s "trg MCP Credentials" -a <name>
```

`trg mcp auth logout` is idempotent when no entry exists. The raw
`security delete-generic-password` command typically exits with a non-zero
status if there is no matching item; scripts should account for that.

### Limitations

- **Platform**: the `keychain` backend is macOS only. The `openbao` backend
  works anywhere `trg` runs.
- **Interactive only**: a TTY on stdin and stderr is required for the
  browser handshake. Headless environments fail with
  `stdin/stderr is not a TTY; OAuth requires an interactive browser session`.
- **No multi-instance coordination**: if two `trg mcp proxy` children for
  the same server start at the same instant with an empty Keychain entry,
  two browser tabs may open. Subsequent invocations are silent.

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

### OAuth credentials in OpenBao, one server still on the Keychain

```toml
[secrets.backends.work]
kind = "openbao"
addr = { env = "BAO_ADDR", default = "http://127.0.0.1:8200" }
mount = "secret"
path_prefix = "trg"
token_file = "~/.vault-token"

[mcp.servers.internal]
url = "https://mcp.internal.example.com/mcp"
secrets = "work"

[mcp.servers.github]
url = "https://api.githubcopilot.com/mcp/"
```

`internal` stores its OAuth credentials at
`secret/data/trg/mcp/<hostname>/internal`. `github` names no backend, so it
keeps using the macOS Keychain exactly as it did before `[secrets]` existed.

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
| `<name> is not a declared secrets backend; declared: ...` | A server's `secrets` field names a backend with no `[secrets.backends.<name>]` entry. |
| `declare exactly one of token_file or token, not ...` | The openbao backend declared neither token source, or both. |
| `the token file at <path> is readable by other users` | The token file's mode grants group or other access. Run `chmod 600`. |
| `OpenBao at <addr> has no <mount> mount, or it is not a KV v2 mount` | The mount name is wrong, or the mount is KV v1. |
| `OpenBao rejected the token (...); run bao login and retry` | The token is absent, expired, or lacks a policy for the path. |
