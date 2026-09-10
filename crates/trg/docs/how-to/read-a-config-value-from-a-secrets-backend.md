# Read a config value from a secrets backend

`trg mcp auth login` puts an MCP server's OAuth credentials in a backend on its
own. This guide covers the other kind of secret: a value you hold, such as a
long-lived API token, that a server's config needs at startup.

Declaring it as a var means the value never has to be exported into the
environment of whatever launches `trg`, and never has to sit in the config file.

## Before you start

You need a `[secrets.backends.<name>]` entry that `trg doctor` reports as
healthy. See
[Store MCP OAuth credentials in OpenBao](use-openbao-as-a-secrets-backend.md)
for setting one up, or use the macOS Keychain:

```toml
[secrets.backends.local]
kind = "keychain"
```

## 1. Write the value

The value comes from stdin, never from a flag, so it stays out of `argv` where
anything else on the machine could read it out of `ps`, and out of the shell
history:

```sh
op read --account my.1password.com "op://Private/memorizer/token" | trg secret put \
  --backend local --path mcp/memorizer --key token
```

```text
wrote `token` at `mcp/memorizer` in `local`
declare it with: { backend = "local", path = "mcp/memorizer", key = "token" }
```

One trailing newline is stripped, because that one is the shell's rather than
the secret's. An empty value is refused, since a command that failed upstream
is the usual reason for one.

A value that already lives in 1Password does not have to be copied like this
at all: see
[If the value already lives in 1Password](#if-the-value-already-lives-in-1password).

Piping from a password manager is the safe form. A heredoc is not: bash records
heredoc bodies in `HISTFILE` like any other input, so typing the value inline
puts it in the history file. If you have no manager to pipe from, turn history
off for the duration:

```sh
set +o history
trg secret put --backend local --path mcp/memorizer --key token <<'EOF'
the-token
EOF
set -o history
```

## 2. Declare it in the server

Paste back the line the command printed:

```toml
[mcp.servers.memorizer]
url = "https://mcp.example.com/mcp"

[mcp.servers.memorizer.vars]
token = { backend = "local", path = "mcp/memorizer", key = "token" }

[mcp.servers.memorizer.headers]
Authorization = ["Bearer ", { var = "token" }]
```

The declaration is the secret's whole identity. It names nothing about
which server reads it, so the same inline table can be pasted into as many
servers as need that value, and a server that names a `secrets` backend for its
own OAuth credentials can still read vars from a different one.

## 3. Check it reads back

```sh
trg secret get --backend local --path mcp/memorizer --key token
```

The value is printed raw, with a trailing newline only when a terminal is
looking at it, so it pipes cleanly:

```sh
trg secret get --backend local --path mcp/memorizer --key token | wc -c
```

Then start the server as usual:

```sh
trg mcp proxy --server memorizer
```

## Where the path lands

`--path` is relative to the backend's own layout, the same as the paths
`trg mcp auth login` writes. For OpenBao that is
`<mount>/data/<path_prefix>/<owner>/<path>`, so with the backend above,
`--path mcp/memorizer` is:

```sh
bao kv get kv/trg/alice/mcp/memorizer
```

The Keychain has no prefix, so the path is the account name verbatim.

That layout means a var can share an entry with a server's OAuth credentials:
`trg mcp auth login --server memorizer` stores under the key `credentials` at
`mcp/memorizer`, and both commands read, modify, and write, so neither takes
the other's keys with it. `trg mcp auth logout` likewise removes only
`credentials`.

## Several values from one entry

An entry holds a map, so related values belong together at one path:

```sh
printf '%s' "$ID"     | trg secret put --backend local --path mcp/memorizer --key client_id
printf '%s' "$SECRET" | trg secret put --backend local --path mcp/memorizer --key client_secret
```

```toml trg-example=fragment
[mcp.servers.memorizer.vars]
client_id     = { backend = "local", path = "mcp/memorizer", key = "client_id" }
client_secret = { backend = "local", path = "mcp/memorizer", key = "client_secret" }
```

Vars naming the same backend and path are read together. A KV v2 read answers
with the whole entry at a path, so those two cost one round trip between them,
not two. `put` preserves the keys it did not write, so the second command above
leaves `client_id` alone.

## If the value already lives in 1Password

Steps 1 to 3 copy a value into a backend `trg` can write to. A `onepassword`
backend skips the copy: `trg` reads the item where it already is, so there is
nothing to write and nothing to keep in sync afterwards.

```toml
[secrets.backends.personal]
kind    = "onepassword"
account = "my.1password.com"
```

Address the value with the reference the item's `Copy Secret Reference` button
puts on your clipboard, as a var's `ref`:

```toml trg-example=fragment
[mcp.servers.memorizer.vars]
token = { backend = "personal", ref = "op://Private/memorizer/token" }
```

A field inside a section takes one more segment,
`op://<vault>/<item>/<section>/<field>`. That segment is optional when the
field name is unique across the item, and the field may be named by its id
instead of its label; matching ignores case either way. Check it the same way
step 3 does, with `--ref` in place of `--path` and `--key`:

```sh
trg secret get --backend personal --ref "op://Private/memorizer/token"
```

References into the same item are read together, the way vars sharing a path
are: `op item get` answers with every field on the item, so four references
into one item cost one round trip between them, not four.

This backend is read-only. `trg secret put` against it fails rather than
writing, because the item is managed in 1Password itself. `path` and `key` are
not a second spelling of a reference either: a var that uses them here fails to
load.

## Troubleshooting

**The backend is down and I need to reset a server's credentials**

`trg mcp auth status` and `trg mcp auth logout` do not resolve the endpoint, so
they never read a var. They keep working when the backend a var names is
unreachable. `trg mcp proxy` and `trg mcp auth login` do need the endpoint and
will report the var they could not read.

**``var ... found nothing at that path``**

Nothing has been written there. The message ends with the exact
`trg secret put` that writes it.

**``var ... found no such key there (that entry holds: ...)``**

The path exists but holds different keys, usually a typo in one of them. The
message lists the key names that are there; it never lists their values.

**``the value is read from stdin, so pipe it in``**

`trg secret put` was run with a terminal on stdin. Pipe the value in or use a
heredoc.

**``` `addr` cannot come from a secrets backend, because it is what reaching one requires ```**

A `[secrets.backends.*]` entry's `addr` or `token` used a secret var of any
shape. Reaching a backend cannot depend on having already reached it. Use a
literal, `{ env = "..." }`, or `token_file`.

**``... which is addressed with a 1Password secret reference, not `path`/`key` ``**

The var names a `onepassword` backend but was written with `path` and `key`.
The message spells out the `ref` line to replace it with. The mirror case, a
`ref` naming a `keychain` or `openbao` backend, reports the same thing in
reverse.

## See also

- [Config reference: variables](../reference/config.md#variables-mcpserversnamevars)
- [Store MCP OAuth credentials in OpenBao](use-openbao-as-a-secrets-backend.md)
- [Why secrets backends are addressed, not searched](../explanation/secrets-backends.md)
