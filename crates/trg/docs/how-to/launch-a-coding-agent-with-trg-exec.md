# Launch a coding agent with `trg exec`

`trg exec run <name>` execs into whatever command a `[exec.<name>]` entry
names, with its environment resolved the same way an MCP server's `vars` are.
This guide covers declaring an entry, listing what's declared, and running
one.

## 1. Declare an entry

```toml
[exec.claude]
command = "claude"
args    = ["--dangerously-skip-permissions"]
```

`command` is looked up on `PATH` the same as a shell would. `args` is
optional and comes before anything typed after `<name>` on the command line.

## 2. See what's declared

```sh
trg exec list
```

Prints one name per line — `claude`, here. `list` sits beside `run` rather
than being folded into `trg exec <name>` directly, so an entry can be named
anything (including, one day, `run` or `list` itself) without colliding with
the verb that lists it.

## 3. Run it

```sh
trg exec run claude
```

This replaces the `trg` process (`exec(2)`): same pid, no `trg` left running
underneath once the command starts. Anything meant for the launched command
goes after `--`, and is appended after the entry's own `args`:

```sh
trg exec run claude -- --resume
```

The `--` is required, not optional. Without it, there is no way to tell a
typo in one of `trg exec run`'s own flags (`--evn` instead of `--env`) apart
from a flag meant for the launched command — both look like an unrecognized
token. `--` draws that line explicitly: everything before it is validated as
`trg exec run`'s own flags (`--env`, `--unset`, `--output-format`), and an
unrecognized one there is a hard error, the same as anywhere else in `trg`.
Everything after `--` is handed to the launched command untouched, with no
validation at all.

## Resolving env

An entry's `env` table accepts the same shapes an MCP server's `vars` do, see
[Config reference: Variables](../reference/config.md#variables-mcpserversnamevars)
for the full rules:

```toml
[exec.claude]
command = "claude"
unset   = ["ANTHROPIC_API_KEY"]

[exec.claude.env]
MODE                  = "prod"
ANTHROPIC_AUTH_TOKEN  = { backend = "homelab", path = "exec/claude", key = "token" }
```

- A literal string is used as-is.
- `{ env = "NAME", default = "..." }` reads it from `trg`'s own environment.
- `{ backend = "...", path = "...", key = "..." }` reads it from a
  `keychain` or `openbao` `[secrets.backends.<name>]` entry at load time, same
  as
  [Read a config value from a secrets backend](read-a-config-value-from-a-secrets-backend.md).
- `{ backend = "...", ref = "op://<vault>/<item>/<field>" }` reads it from a
  `onepassword` entry, addressed by the reference that item's `Copy Secret
  Reference` button yields. Each kind takes only its own spelling: `path` and
  `key` against a `onepassword` backend, or a `ref` against either of the
  others, fails to load.
- An array of any of those concatenates them in order, e.g.
  `dir = [{ env = "HOME" }, "/app/state"]` resolves to
  `<value of $HOME>/app/state`. This composition is available only for
  `env` — see
  [Config reference: Composition in `[exec.<name>.env]`](../reference/config.md#composition-in-execnameenv).

`unset` removes an inherited variable before `env` is applied — use it for a
credential the entry's own auth should own instead, such as an API key the
launched tool would otherwise pick up ahead of an OAuth session it manages
itself.

## One-off overrides

`--env KEY=VALUE` and `--unset KEY` both work on the command line too,
applied after the entry's own `unset`/`env`:

```sh
trg exec run claude --env DEBUG=1 --unset SOME_STALE_VAR
```

`--env` is for a literal like `DEBUG=1`, never a secret: like every other
flag, it lands in `argv` and shell history. A value that needs to stay out of
both belongs in the entry's `env` table as a `{ backend = ... }` entry.

## Troubleshooting

**``no [exec] entries in config``**

No `[exec.<name>]` entry exists yet. Add one as shown above.

**``unknown exec entry <name> — known: ...``**

`<name>` does not match any declared entry. The message lists the ones that
are.

**The launched command needs a value this doesn't resolve**

Everything the entry's `env` table can express is documented in
[Config reference: `[exec.<name>]`](../reference/config.md#execname).
If a value needs to be composed from several pieces (a directory under
`$HOME`, say), declare it as an array and each piece concatenates in order:

```toml
CLAUDE_SECURESTORAGE_CONFIG_DIR = [{ env = "HOME" }, "/app/state"]
```

See
[Config reference: Composition in `[exec.<name>.env]`](../reference/config.md#composition-in-execnameenv)
for the full rules.

## See also

- [Config reference: `[exec.<name>]`](../reference/config.md#execname)
- [Read a config value from a secrets backend](read-a-config-value-from-a-secrets-backend.md)
