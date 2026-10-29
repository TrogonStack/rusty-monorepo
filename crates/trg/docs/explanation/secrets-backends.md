# Secrets backends

`trg mcp` obtains OAuth credentials for a remote MCP server and has to keep
them somewhere. This page explains the shape of that "somewhere" and why it
looks the way it does.

## Addressed, never searched

A server names exactly one backend:

```toml trg-example=fragment
[mcp.servers.internal]
secrets = "work"
```

There is no ordered list of backends, no probing, and no fallback to a second
backend when the first one fails. The one exception is narrow and is written
out, value by value, in
[A read can name its own fallbacks](#a-read-can-name-its-own-fallbacks).

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

## A read can name its own fallbacks

The rule above exists to keep two machines from disagreeing about where a
credential lives. That risk belongs to credentials `trg` writes. A value you
hold and only read, such as an API token or a signing key, has no refresh token
to lose, so for those one declaration may add `fallbacks`, the addresses to
try in order after its own:

```toml trg-example=fragment
[exec.deploy.env]
GH_TOKEN = { backend = "homelab", path = "exec/github", key = "GH_TOKEN", fallbacks = [
  { backend = "personal", ref = "op://Ops/github/GH_TOKEN" },
] }
```

The exception is held to what the design needs.

- It is opt-in per value. Nothing is searched unless that value's declaration
  says so, and the `fallbacks` are written in the config, so "where does this value
  live" is still answered by reading it.
- It is reads only. The `secrets = "<name>"` field that places OAuth
  credentials takes one name and has no `fallbacks`, `trg secret put` takes one
  address, and a backend's own `addr` and `token` cannot use them. A write
  cannot fall through because there is nowhere in the grammar to ask for it.
- The declaration itself is the first address and each entry of `fallbacks`
  is a full address, because a 1Password reference and an OpenBao
  `path` and `key` are not shaped alike. Entries cannot carry `fallbacks` of their own.
- An empty `fallbacks`, or an entry naming an undeclared backend, fails to load.

A secret read by a script can be declared once under `[secrets.vars.<name>]`
with the same table, and read with `trg secret get --name <name>`. The name
adds no new rule: it is the same declaration, held under a name instead of
inside a server.

### Which failures fall through

The test is whether the backend could not answer, or answered with something
that cannot be trusted.

| Falls through to the next address | Stops the read |
| --- | --- |
| Not found: no entry, no key, or no item | A value that cannot be decoded |
| Unreachable, or a transport failure | A 1Password field name that matches more than one field |
| Rejected credentials, expired or denied | A backend that cannot do the operation at all |
| A backend that cannot be set up on this machine, for example its `addr` names an unset environment variable, or its `ca_cert_file` cannot be opened | A backend that is undeclared, or misconfigured the same way on every machine, such as a malformed value or a `ca_cert_file` that is read but is not a certificate |

Not found falls through because a list is itself the statement that the first
store may not hold the value yet, as while values are being copied from one
store to another. The warning and the trace below are what make a typo in the
first address visible rather than silently masked.

OpenBao answers an expired token and an ACL denial with the same status, so
the two cannot be told apart and both fall through.

A value that is present but malformed stops the read, because falling back
would hide a corrupt store. An ambiguous 1Password field stops it, because
picking the other address would choose a secret by luck. A configuration
mistake stops it because it is wrong on every machine, and a fallback that
happened to answer would hide it for as long as the fallback kept working.

### Which backend answered is never silent

The failure the rule guards against is a read where nothing says which store
answered. So a fallback that answers writes a warning to stderr naming the
backend that could not answer, why, and the backend that did. `trg secret get`
keeps stdout to the value alone. The `resolve secret` span records
`trg.secrets.answered_by` and `trg.secrets.fallback.used` whether or not a
fallback was needed.

When every address fails, the error lists each attempted backend and why.

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

## A var is written in its backend's own vocabulary

The credentials `trg` stores are addressed by a layout `trg` chose (below). A
value someone else put in a backend is not. It already has an address, and
that address is whatever the product holding it uses.

So there is no single `{ path, key }` spelling imposed on every kind. A
`keychain` or `openbao` var is a path and a key, because that is what those
stores are. A `onepassword` var is a secret reference,
`op://<vault>/<item>/<field>`, because that is what 1Password's own
`Copy Secret Reference` button puts on the clipboard. Pasting it in unchanged
is both less to get wrong and the only spelling that can reach a field inside
a section at all, which a vault-and-item path plus a field label could not
express.

The cost is that the two are not interchangeable: a var written in the wrong
one fails to load rather than being translated. Translating would mean
inventing a vocabulary native to no backend, and would make a config's address
something you cannot look up in the documentation of the product the secret
lives in.

## A mirror is a declaration, not a copy

A value that lives in one backend and is needed in another could be copied with
`trg secret put` in a pipeline. That works once, and the pipeline is then the
only record that the two are meant to agree.

A `[secrets.mirrors.<name>]` table keeps the source and its targets in the
config instead, so running `trg secret mirror` again converges on the same
state. The source is read once however many targets it has, and each target
succeeds or fails on its own. It is one-way on
purpose: the source is the source of truth, and nothing is ever read back from
the target as an input. That is also why a mirror cannot target a `onepassword`
backend, which `trg` does not write to.

Agreement is judged by a fingerprint of each value, so the comparison never
prints or logs either one, and it runs against the whole config at load time so
a mirror that could never apply is refused while the file is in view.

## Each backend owns its own path layout

| Backend    | Where one server's credentials live                       |
| ---------- | ---------------------------------------------------------- |
| `keychain` | Service = the backend's `service`, account = the server name. |
| `openbao`  | `<mount>/data/<path_prefix>/[<owner>/]mcp/[<machine_id>/]<server-name>` |

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

## `owner` and `machine_id` are two axes, not one

They look like the same idea spelled twice, and collapsing them is the obvious
simplification to reach for. It does not work, because they answer different
questions.

`owner` answers *whose credential is this*. It is an authorization boundary,
and its unit is the person or the service account. `machine_id` answers *which
holder may refresh this*. It is a concurrency hazard, and its unit is the
installation.

The case that separates them is one person on two machines. That is a single
owner and two holders. Against a provider that rotates refresh tokens and
treats a reused one as replay, those two machines revoke each other's grant
even though they belong to the same person and are equally entitled to it. So
keying isolation on identity does not merely fail to help, it guarantees the
collision by putting both machines on one path.

Falling back from one to the other would be worse still. The path would then
depend on which field happened to be set, so "where does this credential live"
would stop being answerable from the config, which is the same defect this
design rejects in searched backends.

## `owner` names a location, it does not enforce one

`owner` is a literal. It does not expand `{{identity.entity...}}` or anything
else, because `trg` has no idea who the token belongs to. The token is opaque
to it: `trg` reads a file and sets a header, and never calls `lookup-self` or
touches `auth/` at all. Resolving an identity in order to build a path would
add a round trip before every operation, and a bootstrap dependency on the
very thing the path is supposed to address.

It would also be the wrong place for it. A boundary that the client computes
is a boundary the client can be edited out of. OpenBao already templates ACL
paths on the caller's identity, and that check runs on the server whatever the
config says, so a wrong `owner` is a `permission denied` rather than a read of
someone else's secret. Duplicating the rule in `trg` would buy nothing and
give two places for it to be wrong.

`owner` sits ahead of `mcp/` in the path so that granting a person their
subtree covers everything `trg` keeps for them, rather than having to be
re-granted for each kind of thing it later stores.

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
- [Read a config value from a secrets backend](../how-to/read-a-config-value-from-a-secrets-backend.md)
- [Mirror a secret into another backend](../how-to/mirror-a-secret-into-another-backend.md)
