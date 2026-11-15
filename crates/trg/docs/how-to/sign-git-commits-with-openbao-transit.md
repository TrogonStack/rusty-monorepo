# Sign git commits with OpenBao Transit

Git signs a commit by running `gpg.ssh.program` as `ssh-keygen -Y sign`. This
guide makes `trg` that program, so the signing key lives in an
[OpenBao](https://openbao.org) Transit engine instead of on the machine, with
an SSH agent such as 1Password's as the fallback (or the first choice).

## Before you start

You need:

- An OpenBao instance with the Transit engine mounted, and a token allowed to
  read `transit/keys/<name>` and update `transit/sign/<name>`.
- A `[secrets.backends.<name>]` of `kind = "openbao"` pointing at it. See
  [Store MCP OAuth credentials in OpenBao](use-openbao-as-a-secrets-backend.md).
- `ssh-keygen` on your `PATH`. `trg` hands it every `-Y` mode other than
  `sign`, so verification keeps working.

## 1. Create an Ed25519 Transit key

```sh
bao write transit/keys/git-personal type=ed25519
bao read -field=keys transit/keys/git-personal
```

Convert the `public_key` it reports to an `ssh-ed25519` line, or reuse a key
you already have. To sign with an existing key, such as one held in 1Password,
import it instead of creating one. This is a manual step `trg` does not do:

```sh
bao write transit/keys/git-personal/import type=ed25519 \
  ciphertext=@wrapped-key.b64
```

See the OpenBao Transit import documentation for how to wrap the key.

## 2. Declare the signer

```toml
[secrets.backends.homelab]
kind = "openbao"
addr = "https://bao.example.com:8200"
mount = "secret"
path_prefix = "trg"
owner = "me"
token_file = "~/.config/trg/bao-token"

[git.ssh.signers.personal]
public_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIF90u+fxY15X29x2s3wSRElNouutw3GGhm7A7ZE4UrAI docs-example"
signer = { backend = "homelab", transit_key = "git-personal", fallbacks = [
  { ssh_agent = "~/Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock" },
] }
```

To prefer the agent and keep Transit as the fallback, swap the order:

```toml
[secrets.backends.homelab]
kind = "openbao"
addr = "https://bao.example.com:8200"
mount = "secret"
path_prefix = "trg"
owner = "me"
token_file = "~/.config/trg/bao-token"

[git.ssh.signers.personal]
public_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIF90u+fxY15X29x2s3wSRElNouutw3GGhm7A7ZE4UrAI docs-example"
signer = { ssh_agent = "~/Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock", fallbacks = [
  { backend = "homelab", transit_key = "git-personal" },
] }
```

## 3. Name `trg` as the signing program

Git runs the program directly, so give `trg` a name it answers to as the
signing program:

```sh
ln -s "$(command -v trg)" ~/.local/bin/trg-git-ssh-sign
```

```sh
git config --global gpg.format ssh
git config --global gpg.ssh.program trg-git-ssh-sign
git config --global user.signingkey "key::ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIF90u+fxY15X29x2s3wSRElNouutw3GGhm7A7ZE4UrAI"
git config --global commit.gpgsign true
```

Git also needs an allowed signers file to verify:

```sh
git config --global gpg.ssh.allowedSignersFile ~/.config/git/allowed_signers
```

## 4. Check it

```sh
git commit --allow-empty -S -m "signed through Transit"
git verify-commit HEAD
```

The same signing code is reachable without the symlink:

```sh
trg git ssh sign -n git -f ~/.ssh/id_ed25519.pub notes.txt
```

## What happens when a signer fails

An address falls through to the next only when it could not be asked, never
when it refused. A person who denies an approval prompt must not have the
request signed by another key holder behind their back.

| Address | Outcome | Next |
|---|---|---|
| Transit | Backend unreachable, timeout, or a transport error | Falls through |
| Transit | HTTP 403, so the token was rejected | Falls through |
| Transit | Key missing, not `ed25519`, or its public key differs from `public_key` | Stops |
| Transit | Malformed response | Stops |
| SSH agent | Socket missing or connection refused | Falls through |
| SSH agent | Key not listed by the agent | Falls through |
| SSH agent | Signing fails after the key was listed, such as a denied prompt | Stops |

When a fallback signs, `trg` prints a warning on stderr that names the signer,
why the earlier address failed, and which address answered.

See the [config reference](../reference/config.md#gitsshsignersname) for every
field.
