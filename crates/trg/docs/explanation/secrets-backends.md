# Secrets backends

`trg mcp` obtains OAuth credentials for a remote MCP server and has to keep
them somewhere. This page explains the shape of that "somewhere" and why it
looks the way it does.

## Addressed, never searched

A server names exactly one backend:

```toml
[mcp.servers.internal]
secrets = "work"
```

There is no ordered list of backends, no probing, and no fallback to a second
backend when the first one fails.

The alternative, a search order, is how a lot of credential tooling works, and
it has a failure mode worth avoiding. When a read misses in the first backend
and hits in the second, nothing tells you which one answered. Two machines can
then disagree about where a credential lives while both appear to work, until
one of them silently writes a refresh token to the wrong place and the other
gets logged out. Naming one backend makes "where is this credential" a
question the config answers.

Failure follows the same rule. If the named backend is unreachable, the
command fails saying so. It does not quietly fall back to the Keychain and
leave you with two divergent copies of a rotating credential.

## Declaring is not connecting

Backends resolve lazily. Declaring `[secrets.backends.work]` costs nothing
until a server addresses it, so a laptop that cannot reach the company OpenBao
instance can still use every server that names a different backend, or names
none at all.

This matters because the config file is shared across machines more often than
the network topology is.

## Not naming a backend is a default, not a fallback

A server with no `secrets` field gets the macOS Keychain under the service
name `trg MCP Credentials`. That is exactly what every server got before
`[secrets]` existed, so existing configs keep working and existing Keychain
items stay addressable.

It is a default because it applies when nothing was chosen. It is not a
fallback because nothing ever arrives there after a named backend fails.

## Bootstrap stays acyclic

The backend fields that resolve at all (`addr` and `token`) accept a literal
value or `{ env = "..." }`, never `{ secret = "..." }`; every other field is a
literal. Nothing needed to reach the secret store may itself live in the secret
store.

## Each backend owns its own path layout

| Backend    | Where one server's credentials live                       |
| ---------- | ---------------------------------------------------------- |
| `keychain` | Service = the backend's `service`, account = the server name. |
| `openbao`  | `<mount>/data/<path_prefix>/mcp/<server-name>`, plus `<machine_id>/` when declared |

The Keychain is already scoped to one machine and one login keychain, so it
addresses items by bare server name. Adding a machine segment there would
orphan every credential stored before `[secrets]` existed and buy nothing.

OpenBao shares by default, because reaching one credential from everywhere is
the reason to leave the Keychain at all. Isolation is the opt-in, not the
other way around, for two reasons. A derived default cannot be trusted: two
hosts can answer `hostname -s` identically and silently collide, while an
ephemeral host answers differently on every run, so a container or a CI job
would find an empty path and need a browser flow it cannot run. And the
hazard isolation protects against is narrow. It bites only where a provider
both rotates refresh tokens and treats a reused one as replay, which revokes
the whole grant family. Declaring `machine_id` is how you avoid that, and the
cost you accept for it is one login per machine per server.

## Why the OpenBao client is not a client crate

The backend speaks the KV v2 HTTP API directly through `reqwest`. The surface
it needs is five endpoints. A client crate would add a dependency tree, its
own error taxonomy to translate into `trg`'s, and its own retry and token
renewal policies, none of which this design wants.

Two behaviours in particular are deliberate and would be awkward to keep
through a client crate:

**The token is re-read on every operation.** `trg mcp proxy` is a long-lived
child of an editor. Reading the token once at startup would mean an expiring
token could only be recovered by restarting the editor. Re-reading makes
`bao login` in any terminal enough. `trg` never persists, caches, or renews
the token itself.

**A missing secret and a missing mount are told apart by the body.** OpenBao
answers `404` for both. Only the `errors` array separates them: empty means no
such secret, non-empty means no such mount. Collapsing the two would report a
typo in `mount` as "you are not logged in", which sends you to the wrong fix.

## Failures are classified, never echoed

Only OpenBao's own `errors` array is ever surfaced in a `trg` error message.
The rest of a response body may hold secret material, so an unclassified
status reports the status code and nothing else.

Likewise, a token file that other users can read is refused rather than used.
Silently accepting it would hide a live credential leak behind a working
command.

## See also

- [Config reference: `[secrets.backends.<name>]`](../reference/config.md#secretsbackendsname)
- [Store MCP OAuth credentials in OpenBao](../how-to/use-openbao-as-a-secrets-backend.md)
