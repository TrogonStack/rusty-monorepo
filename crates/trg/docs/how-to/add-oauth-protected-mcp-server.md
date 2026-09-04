# Add an OAuth-protected MCP server (Linear)

This walks through using `trg mcp proxy` for an OAuth-protected MCP HTTP
endpoint. Linear is the worked example; the same shape applies to Notion,
Atlassian, Cloudflare, and any other server that advertises OAuth via
RFC 9728 / RFC 8414 discovery.

> Credentials go to the macOS Keychain unless the server names a different
> backend. To keep them in OpenBao instead, see
> [Store MCP OAuth credentials in OpenBao](use-openbao-as-a-secrets-backend.md).

## 1. Add the server to `config.toml`

```toml
[mcp.servers.linear]
url = "https://mcp.linear.app/mcp"
```

That is the entire entry. No `headers`, no `vars` — OAuth is engaged
automatically when discovery succeeds and no static `Authorization` header
is present.

## 2. Wire it into your MCP host

For Cursor (`~/.cursor/mcp.json` or equivalent):

```json
{
  "mcpServers": {
    "linear": {
      "command": "trg",
      "args": ["mcp", "proxy", "--server", "linear"]
    }
  }
}
```

Claude Desktop, Zed, VS Code, etc. follow the same `command + args` shape.

## 3. First run: interactive auth

Authenticate once from a real terminal:

```sh
trg mcp auth login --server linear
```

The command reports which backend it wrote to. You can also skip this and let
the first `trg mcp proxy` request trigger the flow, but only if the host
spawns it with a TTY attached.

Either way the flow is:

1. `trg` discovers Linear's authorization server metadata.
2. It binds an ephemeral loopback port (`127.0.0.1:<rand>/oauth/callback`).
3. The system browser opens at Linear's authorization page. (The URL is
   also printed on stderr — you can paste it manually if `open(1)` cannot
   launch a browser.)
4. After you approve, Linear redirects back to the loopback listener;
   `trg` exchanges the code for tokens and stores them in the server's
   secrets backend.
5. The proxy then completes the MCP handshake with the access token in
   the `Authorization` header on every request.

Subsequent runs read the token from that backend. No browser, no prompt.

## 4. Verify what was stored

```sh
trg mcp auth status --server linear
```

It names the backend, and prints expiry and granted scopes. Tokens are never
printed. For a machine-readable summary:

```sh
trg mcp auth status --server linear --format json
```

For a Keychain-backed server you can also look directly:

```sh
security find-generic-password -s "trg MCP Credentials" -a linear
```

The secret itself is the JSON-encoded `StoredCredentials` payload.
`security` prints metadata only, not the secret, unless you add `-w`.

## 5. Re-auth or wipe credentials

Token expired without a refresh token, or you want to switch accounts:

```sh
trg mcp auth logout --server linear
```

The next `trg mcp proxy --server linear` triggers a fresh interactive
flow.

Equivalent low-level command, for a Keychain-backed server:

```sh
security delete-generic-password -s "trg MCP Credentials" -a linear
```

`trg mcp auth logout` is idempotent: if nothing is stored, it is a no-op.
`security delete-generic-password` usually exits non-zero when no matching
Keychain item exists; treat that as “already logged out” only if you are
scripting around it (for example `|| true`).

## Troubleshooting

| Symptom                                                                            | Likely cause                                                                                              |
| ---------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------- |
| `stdin/stderr is not a TTY; OAuth requires an interactive browser session`         | First-time auth was attempted from a non-TTY context. Run `trg mcp proxy --server linear` once in a real terminal to seed the Keychain. |
| `timed out waiting for the OAuth callback after 300s`                              | Browser never returned. Re-run; ensure no firewall is blocking the loopback listener.                     |
| `authorization provider returned an error: access_denied`                          | You declined consent on the provider's page. Re-run and approve.                                          |
| `OAuth state mismatch (csrf protection)`                                           | Stale browser tab from a previous run hit the listener. Close the old tab and re-run.                     |
| Provider still says "auth required" after a successful flow                        | The backend may hold a previous run's expired refresh token. Run `trg mcp auth logout --server linear`.  |
| `is not a declared secrets backend`                                                | The server's `secrets` field names a backend with no `[secrets.backends.<name>]` entry.                   |
