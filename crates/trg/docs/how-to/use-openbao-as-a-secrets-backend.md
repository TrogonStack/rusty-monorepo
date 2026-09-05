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
owner = "alice"
token_file = "~/.vault-token"
```

`owner` is required and is your own name on the instance. It is the segment an
ACL is templated on, so a config that left it out would put everyone's
credentials in one subtree and let the second person to log in rotate the
first's refresh token. Nothing derives it for you: it expands nothing, so each
person's config carries their own name.

## 4. Point a server at it

```toml
[mcp.servers.internal]
url = "https://mcp.internal.example.com/mcp"
secrets = "work"
```

Servers that do not name a `secrets` backend keep using the Keychain, so you
can migrate one server at a time.

## 5. Check the backend before logging in

```sh
trg doctor --backend work
```

```text
backend  work (openbao)
target   https://bao.internal.example.com:8200 (mount `secret`, subtree `trg`)

  ok       token          file `/home/alice/.vault-token`
  ok       instance       unsealed, active (OpenBao 2.6.2)
  ok       mount          `secret` answers as KV v2
  ok       subtree        listable, nothing stored yet
```

Each failing check carries what to do about it on the line below it, and the
command exits non-zero if any failed. It only reads, so it is safe to run
against a production instance.

The mount and subtree are probed with the same list `trg` itself issues rather
than with `sys/mounts`, so a correctly scoped token is enough. That also means a
token whose policy stops at its own subtree is denied before OpenBao looks the
mount up, and the mount check reports `skipped` rather than claiming a mount it
never reached.

Run it without `--backend` to check every declared backend at once.

For a script, `--format json` prints the same report:

```sh
trg doctor --backend work --format json
```

## 6. Log in to the MCP server

```sh
trg mcp auth login --server internal
```

The command names the backend it wrote to. Confirm it landed:

```sh
trg mcp auth status --server internal
bao kv get secret/trg/alice/mcp/internal
```

Every machine reading that path now uses the same credential. To give this
machine its own instead, add `machine_id = "laptop"` to the backend and log in
again; the entry moves to `secret/trg/alice/mcp/laptop/internal`.

## Letting the server enforce the subtree

`owner` puts each person's credentials in their own subtree, but the policy in
step 1 still grants everyone holding it all of `trg/*`, so the separation is one
every config is trusted to respect. On a shared instance, template the path on
the caller's identity as well, and the server enforces it instead.

Template on the caller's *alias* name, which for `userpass` is the username.
It needs the auth mount's accessor, which differs per instance. Set
`AUTH_MOUNT` to the mount your operators actually log in through; the example
assumes `userpass` at its default path, and `jq -e` fails rather than writing a
policy that grants nothing if that mount is not there:

```sh
AUTH_MOUNT=userpass/
ACCESSOR=$(bao auth list -format=json | jq -er --arg m "$AUTH_MOUNT" '.[$m].accessor')

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

The `owner` each user already declared in step 3 is what has to match the alias
name in that policy. Alice's `owner = "alice"` stores at
`secret/trg/alice/mcp/<server>`, which is the subtree the template grants her.

Getting it wrong is then not a way to read someone else's credential: the policy
answers `permission denied` for any subtree but the caller's, and `trg doctor`
reports that as a failing `subtree` check.

> Do not reach for `{{identity.entity.name}}` here. Unless an operator has
> set one, OpenBao generates that name itself, so it comes out as something
> like `entity_1a2b3c4d.root` rather than the username. The alias name is the
> one a person can type into `owner`.

`owner` is about who may read a credential. It is not what keeps two of your
own machines from rotating each other's refresh tokens, since both of them are
the same owner. `machine_id` is the field for that, it stays optional, and the
two compose: `owner = "alice"` with `machine_id = "laptop"` stores at
`secret/trg/alice/mcp/laptop/<server>`.

## Using a private CA

Point `ca_cert_file` at the PEM bundle:

```toml
[secrets.backends.work]
kind = "openbao"
addr = "https://bao.internal.example.com:8200"
mount = "secret"
path_prefix = "trg"
owner = "alice"
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
owner = "ci"
token = { env = "BAO_TOKEN" }
```

Exactly one of `token_file` or `token` may be declared.

## Troubleshooting

Start with `trg doctor`, which narrows most of the below to a single failing
check. It takes `--backend work` to look at one backend rather than every
declared one.

**`OpenBao rejected the token (permission denied); run bao login and retry`**

The token expired, or its policy does not cover the path. Check both:

```sh
bao token lookup
bao kv get secret/trg/alice/mcp/internal
```

`trg` re-reads `token_file` on every operation, so running `bao login` in
another terminal fixes a running `trg mcp proxy` without restarting the editor
that spawned it.

An editor surfaces this one itself. A proxy that cannot reach its credentials
answers the host with the message over MCP rather than exiting quietly, so it
appears wherever that editor shows an MCP server's errors.

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
