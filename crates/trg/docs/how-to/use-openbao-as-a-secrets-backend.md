# Store MCP OAuth credentials in OpenBao

By default `trg` keeps OAuth credentials for an MCP server in the macOS
Keychain. This guide points one server at an [OpenBao](https://openbao.org)
KV v2 mount instead, so a Linux machine, a container, or a second workstation
can reach the same backend.

Every machine pointed at the same path shares one credential, so logging in
once is enough. Some providers rotate refresh tokens and treat a reused one as
a replay, revoking the whole grant; against those, add `machine_id` to give
each machine its own entry. See
[Secrets backends](../explanation/secrets-backends.md).

## Before you start

You need:

- An OpenBao instance you can reach, with a KV v2 mount. This guide uses
  `secret`, which is what `bao server -dev` mounts. A deployed instance often
  mounts KV v2 elsewhere, commonly `kv`. Check before you start, and use that
  name wherever this guide says `secret`:

  ```sh
  bao secrets list
  ```

- Permission to write a policy on it, or someone who can.
- The `bao` CLI on your `PATH`.

## 1. Create a policy for `trg`

`trg` reads and writes secret data, and deletes metadata on logout. Both
paths are required: in KV v2 the data and its version history are addressed
separately, and a policy covering only `data/` leaves `trg mcp auth logout`
unable to remove anything.

```hcl
# trg-mcp.hcl
path "secret/data/trg/*" {
  capabilities = ["create", "read", "update", "patch", "delete"]
}

path "secret/metadata/trg/*" {
  capabilities = ["read", "list", "delete"]
}
```

Load it:

```sh
bao policy write trg-mcp trg-mcp.hcl
```

Adjust `secret` and `trg` to match the `mount` and `path_prefix` you intend to
configure below.

## 2. Log in

```sh
bao login -method=oidc
```

Any auth method works; `trg` only consumes the resulting token. `bao login`
writes the token to `~/.vault-token` with mode `600`.

> `~/.vault-token` is the path the CLI actually uses, including on OpenBao.
> There is no `~/.bao-token`.

## 3. Declare the backend

In `~/.config/trg/config.toml`:

```toml
[secrets.backends.work]
kind = "openbao"
addr = { env = "BAO_ADDR", default = "http://127.0.0.1:8200" }
mount = "secret"
path_prefix = "trg"
token_file = "~/.vault-token"
```

## 4. Point a server at it

```toml
[mcp.servers.internal]
url = "https://mcp.internal.example.com/mcp"
secrets = "work"
```

Servers that do not name a `secrets` backend keep using the Keychain, so you
can migrate one server at a time.

## 5. Log in to the MCP server

```sh
trg mcp auth login --server internal
```

The command names the backend it wrote to. Confirm it landed:

```sh
trg mcp auth status --server internal
bao kv get secret/trg/mcp/internal
```

Every machine reading that path now uses the same credential. To give this
machine its own instead, add `machine_id = "laptop"` to the backend and log in
again; the entry moves to `secret/trg/mcp/laptop/internal`.

## Giving each user their own subtree

The policy in step 1 grants everyone holding it the same `trg/*` subtree. On a
shared instance, template the path on the caller's identity instead, so the
server enforces the boundary rather than trusting every config to stay in its
own lane.

Template on the caller's *alias* name, which for `userpass` is the username.
It needs the auth mount's accessor, which differs per instance:

```sh
ACCESSOR=$(bao auth list -format=json | jq -r '."userpass/".accessor')

cat > trg-mcp.hcl <<EOF
path "secret/data/trg/{{identity.entity.aliases.$ACCESSOR.name}}/*" {
  capabilities = ["create", "read", "update", "patch", "delete"]
}

path "secret/metadata/trg/{{identity.entity.aliases.$ACCESSOR.name}}/*" {
  capabilities = ["read", "list", "delete"]
}
EOF

bao policy write trg-mcp trg-mcp.hcl
```

Each user then names their own subtree in `path_prefix`:

```toml
[secrets.backends.work]
kind = "openbao"
addr = { env = "BAO_ADDR" }
mount = "secret"
path_prefix = "trg/alice"
token_file = "~/.vault-token"
```

`path_prefix` is a literal and expands nothing, so each user's config carries
their own name. Getting it wrong is not a way to read someone else's
credential: the policy answers `permission denied` for any subtree but the
caller's.

> Do not reach for `{{identity.entity.name}}` here. Unless an operator has
> set one, OpenBao generates that name itself, so it comes out as something
> like `entity_1a2b3c4d.root` rather than the username. The alias name is the
> one a person can type into `path_prefix`.

## Using a private CA

Point `ca_cert_file` at the PEM bundle:

```toml
[secrets.backends.work]
kind = "openbao"
addr = "https://bao.internal.example.com:8200"
mount = "secret"
path_prefix = "trg"
token_file = "~/.vault-token"
ca_cert_file = "~/.config/trg/bao-ca.pem"
```

That bundle **replaces** the OS trust store for this backend rather than
adding to it, so a pinned deployment stays pinned. There is no option to skip
verification.

## Passing the token another way

In CI or a container there is no `bao login`. Use `token` instead of
`token_file`:

```toml
[secrets.backends.ci]
kind = "openbao"
addr = { env = "BAO_ADDR" }
mount = "secret"
path_prefix = "trg"
token = { env = "BAO_TOKEN" }
```

Exactly one of `token_file` or `token` may be declared.

## Troubleshooting

**`OpenBao rejected the token (permission denied); run bao login and retry`**

The token expired, or its policy does not cover the path. Check both:

```sh
bao token lookup
bao kv get secret/trg/mcp/internal
```

`trg` re-reads `token_file` on every operation, so running `bao login` in
another terminal fixes a running `trg mcp proxy` without restarting the editor
that spawned it.

**`` `addr` must use `https://` for a remote OpenBao ``**

The token travels in a header, so `trg` sends it in the clear only to a
loopback address. Put TLS in front of a remote instance and use `https://`.
For a private CA, see [Using a private CA](#using-a-private-ca).

**`the token file at ~/.vault-token is readable by other users (mode 0644)`**

`trg` refuses a token file other users can read rather than silently using a
leaked credential:

```sh
chmod 600 ~/.vault-token
```

**``OpenBao at ... has no `secret` mount, or it is not a KV v2 mount``**

The `mount` name is wrong, or the mount is KV v1. Check with:

```sh
bao secrets list -detailed
```

**`could not connect to OpenBao at ...`**

`addr` is unreachable. `trg` gives up after `timeout_ms` (5 seconds by
default) rather than hanging, because a hung `trg mcp proxy` child surfaces to
the editor only as a generic MCP error.

**``a server stored in OpenBao must be named with [A-Za-z0-9._-]``**

The server's `[mcp.servers.<name>]` key becomes a path segment. Rename the
server, or leave it on the Keychain, which accepts any name.

## See also

- [Config reference: `[secrets.backends.<name>]`](../reference/config.md#secretsbackendsname)
- [Why secrets backends are addressed, not searched](../explanation/secrets-backends.md)
