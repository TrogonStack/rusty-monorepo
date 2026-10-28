# Mirror a secret into another backend

A value you keep in one place, such as a deploy token in 1Password, is often
needed in another, such as an OpenBao instance a server reads from. Copying it
by hand with `trg secret put` works once and then drifts.

A mirror declares the pair, so `trg secret mirror` can bring the target in line
with the source whenever the source changes. The source stays the source of
truth: `trg` only ever writes toward the target.

## Before you start

You need two `[secrets.backends.<name>]` entries: a source `trg` can read, and
a target of kind `keychain` or `openbao`. A `onepassword` backend can be a
source but never a target, because its items are managed in 1Password.

```toml
[secrets.backends.personal]
kind    = "onepassword"
account = "my.1password.com"

[secrets.backends.homelab]
kind        = "openbao"
addr        = "https://bao.example.com:8200"
mount       = "kv"
path_prefix = "trg"
owner       = "alice"
token_file  = "~/.vault-token"
```

## 1. Declare the mirror

Each side is addressed the way a var is, in the vocabulary of its backend:

```toml trg-example=fragment
[secrets.mirrors.github-token]
source = { backend = "personal", ref = "op://Engineering/deploy-key/token" }
targets = [
  { backend = "homelab", path = "exec/github", key = "token" },
]
```

An SSH private key is mirrored in OpenSSH format by ending the reference in
`?ssh-format=openssh`, which 1Password's `op read` documents. The target
receives the OpenSSH text, and the `source` key it records is the reference as
written, query included:

```toml trg-example=fragment
[secrets.mirrors.signing-key]
source = { backend = "personal", ref = "op://Engineering/deploy-key/private key?ssh-format=openssh" }
targets = [
  { backend = "homelab", path = "exec/deploy", key = "ssh_private_key" },
]
```

A mirror is named by the table it is declared under, and `targets` lists every
place the one value goes. Declare as many `[secrets.mirrors.<name>]` tables as
you have values. The file is checked as a whole when it loads, so a mirror that
could never apply is reported with its name before anything is read or
written.

## 2. See what would change

```sh
trg secret mirror
```

```text
would create  github-token -> exec/github#token (homelab)
would create  signing-key -> exec/deploy#ssh_private_key (homelab)
2 targets: 0 up to date, 2 would create, 0 would update, 0 failed; pass --apply to write
```

Nothing is written without `--apply`. Each source is read once, every target
is read, and each target reports one of:

| Status         | Meaning                                                               |
| -------------- | --------------------------------------------------------------------- |
| `would create` | The target holds nothing at that key.                                 |
| `would update` | The target holds a different value.                                   |
| `up to date`   | The target already holds the source's value.                          |
| `failed`       | The target could not be read or checked. The reason follows, indented. |

Values are compared by a fingerprint, and are never printed.

## 3. Write the targets

```sh
trg secret mirror --apply
```

```text
created       github-token -> exec/github#token (homelab)
created       signing-key -> exec/deploy#ssh_private_key (homelab)
2 targets: 0 up to date, 2 created, 0 updated, 0 failed
```

To run only some mirrors, name them: `trg secret mirror signing-key --apply`.
A name that is not declared is an error that lists the declared ones.

A target that is already up to date is not rewritten. After a write, `trg`
reads the target back and fails the target if it does not hold what was
written.

Keys at the target path that the mirror did not write are kept. The mirror
adds one of its own, `source`, holding the reference the value came from, so
whoever finds the entry in the backend can tell where to change it.

A target that fails does not stop the others, of its mirror or of the ones
after it, and the exit code is nonzero if any did. A source that cannot be read
fails every target of its mirror.

## Read the mirrored value

The target is an ordinary entry, so a var reads it like any other:

```toml trg-example=fragment
[exec.deploy.env]
GITHUB_TOKEN = { backend = "homelab", path = "exec/github", key = "token" }
```

`trg secret put` still prints `declare it with:` after every write, whether or
not a mirror covers that path. It is a paste-ready var line, not a warning
about an undeclared path, so a mirror declaration does not change it.

## Troubleshooting

**``refusing to mirror an empty source value``**

The source holds an empty string, or a field that is blank in the item. An
empty source is refused even when the target holds an older value, so a
cleared field never wipes a working secret.

**``target backend `...` is of kind `onepassword`, which `trg` only reads from``**

Mirrors only write to `keychain` and `openbao`. Swap the source and the target, or
point the target at a backend `trg` can write to.

**``the target did not read back what was written``**

The backend acknowledged the write but returned something else. Check the
token's policy for the path, then run the mirror again.

## See also

- [Config reference: `[secrets.mirrors.<name>]`](../reference/config.md#secretsmirrorsname)
- [Read a config value from a secrets backend](read-a-config-value-from-a-secrets-backend.md)
- [Why secrets backends are addressed, not searched](../explanation/secrets-backends.md)
