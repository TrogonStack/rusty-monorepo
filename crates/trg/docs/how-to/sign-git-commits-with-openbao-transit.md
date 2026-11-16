# Sign git commits with OpenBao Transit

Git signs a commit by running `gpg.ssh.program` as `ssh-keygen -Y sign`. This
guide makes `trg git ssh sign` that program, so the signing key lives in an
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

## 1. Create a Transit key

`trg` signs with `ed25519`, `rsa-2048`, `rsa-3072` and `rsa-4096` Transit keys.
Transit holds the private half, so the key never needs to be exportable.

```sh
bao write transit/keys/git-personal type=ed25519
```

Transit reports an Ed25519 public key as its 32 raw bytes in base64. Wrap them
in the SSH key format to get the `ssh-ed25519` line the rest of this guide
uses:

```sh
raw=$(bao read -format=json transit/keys/git-personal | jq -r '.data.keys["1"].public_key')
echo "ssh-ed25519 $( { printf '\000\000\000\013ssh-ed25519\000\000\000\040'; printf '%s' "$raw" | base64 -d; } | base64 | tr -d '\n')"
```

That line replaces the example key everywhere below: in `public_key`, in
`user.signingkey` and in the allowed signers entry. A signer whose
`public_key` is not the Transit key refuses to sign and says which key
Transit holds.

### Or mirror a key you already have

To sign with an existing key, such as one held in 1Password, let `trg secret
mirror` import it into Transit instead of creating one. That leaves the same
key in Transit and in the agent, which is what an agent fallback needs.

Declare the Transit key as a mirror target, the way a signer addresses it, and
read the source key in OpenSSH format with `?ssh-format=openssh`:

```toml
[secrets.backends.personal]
kind = "onepassword"
account = "my.1password.com"

[secrets.backends.homelab]
kind = "openbao"
addr = "https://bao.example.com:8200"
mount = "secret"
path_prefix = "trg"
owner = "me"
token_file = "~/.config/trg/bao-token"

[secrets.mirrors.git-personal]
source = { backend = "personal", ref = "op://Engineering/git-signing/private key?ssh-format=openssh" }
targets = [
  { backend = "homelab", transit_key = "git-personal" },
]
```

```sh
trg secret mirror
trg secret mirror apply
```

```text
would create  git-personal -> transit/keys/git-personal (homelab)
1 target: 0 up to date, 1 would create, 0 would update, 0 failed; run `trg secret mirror apply` to write
```

The token needs `read` on `transit/wrapping_key` and `transit/keys/<name>`,
and `update` on `transit/keys/<name>/import` (and `import_version` to change
the key later). `trg` fetches the wrapping key, encrypts the private key for
it so the key is never sent in the clear, and creates the Transit key as
non-exportable with plaintext backup disallowed. It supports `ssh-ed25519` and
`ssh-rsa` keys of 2048, 3072 or 4096 bits, which become `ed25519`,
`rsa-2048`, `rsa-3072` and `rsa-4096`. An ECDSA key, or one protected by a
passphrase, is refused. The `transit_mount` field names a mount other than
`transit`.

Transit never gives a private key back, so the mirror compares public keys. A
Transit key that already holds the source key is reported as up to date and is
not touched. When the source key changes, the new key is added as a new version
of the Transit key, and signing switches to it. OpenBao only allows that for a
key that was itself imported, so a key made by `bao write transit/keys/...`
cannot be replaced this way: OpenBao's refusal is shown as the target's error,
and the fix is to mirror into a new `transit_key`. A key can also not change
type, so an `ed25519` Transit key does not take an RSA source.

See [Mirror a secret into another backend](mirror-a-secret-into-another-backend.md)
for the rest of how mirrors behave.

An RSA key is declared the same way, with its `ssh-rsa` line as the
`public_key`. `trg` reads the key Transit reports and refuses to sign unless it
is the declared one. An RSA signature is made as `rsa-sha2-512`: `trg` asks
Transit for `hash_algorithm=sha2-512` with `signature_algorithm=pkcs1v15`.

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

An `ssh_agent` address signs only when the agent holds the private key for
`public_key`. A key generated in Transit never leaves it, so no agent can hold
it: with a generated key, leave the agent out. An agent fallback needs the
same key on both sides, which means mirroring a key the agent already holds
into Transit, as step 1 describes.

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

## 3. Point git at `trg`

Git runs `gpg.ssh.program` as a single path, without a shell, so the setting
cannot carry the arguments `trg git ssh sign`. Write a small executable file
that adds them. Its name and location are yours to choose:

```sh
#!/bin/sh
exec trg git ssh sign "$@"
```

Save it as `~/.local/bin/git-ssh-sign`, make it executable with
`chmod +x ~/.local/bin/git-ssh-sign`, and point git at it:

```sh
git config --global gpg.format ssh
git config --global gpg.ssh.program ~/.local/bin/git-ssh-sign
git config --global user.signingkey "key::ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIF90u+fxY15X29x2s3wSRElNouutw3GGhm7A7ZE4UrAI"
git config --global commit.gpgsign true
```

Git treats a `user.signingkey` that starts with `ssh-` as the key itself, so
the `key::` prefix is optional. Both forms work, and so does the path of a
`.pub` file, or of a private key that has a `.pub` beside it.

Git also needs an allowed signers file to verify, with an entry naming a
principal, usually your email, and the same public key:

```sh
mkdir -p ~/.config/git
echo "you@example.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIF90u+fxY15X29x2s3wSRElNouutw3GGhm7A7ZE4UrAI" >> ~/.config/git/allowed_signers
git config --global gpg.ssh.allowedSignersFile ~/.config/git/allowed_signers
```

Git looks the signing key up in this file to find its principal, so a key
without an entry fails `git verify-commit`.

## 4. Check it

```sh
git commit --allow-empty -S -m "signed through Transit"
git verify-commit HEAD
```

The same signing code can be run by hand, with or without `-Y sign`:

```sh
trg git ssh sign -n git -f ~/.ssh/id_ed25519.pub notes.txt
```

## Replace a bash `gpg.ssh.program`

A script that signs with a Transit key and falls back to 1Password usually
holds an RSA key imported into Transit without export, and sets a bare
`ssh-rsa` line as `user.signingkey`. [Mirror that key](#or-mirror-a-key-you-already-have)
to import it the same way, as `rsa-4096` and non-exportable, without wrapping
it by hand.

The same setup in `trg` is one signer. Its `public_key` is that bare line, and
its fallback is the 1Password agent socket, which is the socket the
`op-ssh-sign` helper talks to:

```toml
[secrets.backends.homelab]
kind = "openbao"
addr = "https://bao.example.com:8200"
mount = "secret"
path_prefix = "trg"
owner = "me"
token_file = "~/.config/trg/bao-token"
timeout_ms = 3000

[git.ssh.signers.personal]
public_key = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAACAQCdemZz7dZLzntCMvrefRlIe2SCoGoU/wKMc/IFx6FvlIr9cdCl9q+CkaaJ0n3mvmDhVHkWYCWfe8F6P3jjOAy+w+pp6q7yoBFX88rGSafRoP2VUuaqGt+JWYiGY3hHVHzmFXqqSW4dqMZEM7raO6ui9iRet3nqT2CQqOvw3bdO18+iEEjQ2MSAFIKgtoEB28QQPiJeSTmJGie6ry+/UOa9YOQRQlII3SQ0cqxDROLTI1Cjy82epDzinBN5zNeC1Fe0XThaT238OjegNe8C0oOaE0GcT3R3NAyYWzVAvL61fjT5bEkN9Y8/nQIk1yZ1teGd+oW8izP4OUpAC6WfP3DNVv2kmvvwTAPo1fjKcu4jvXg2siMOlImsrr7fLqE83gqm6BSZuMQ0AybwHIht869c1k7De/obxfZFzfzj/Ld/EfTE1/uWcdebiepKJ8z50HLSMyxz+8HZHtCclC1HkP30wbLkbURPnw8Xx8xlAatSZjUHtJLA53lQQM/+TXWx3sz2ZpRIZXs7fnwWZqIgwTPRQAELz4WhLy0adzDTLMh4nVaVcn61nH1p/EKHAQ34Zl1uoPtXTVHUT2nYi8Lh95gODrcM4/eyUfKEPYitRtWWjJhnaXLKlLsK1oeon+hYyPmgnXMRTMY8JCmUBuuIRpwYo+zNY07M2SkXV58RJsVorw== docs-example"
signer = { backend = "homelab", transit_key = "git-personal", fallbacks = [
  { ssh_agent = "~/Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock" },
] }
```

Then point git at a pass-through file as in step 3, and keep the key line you
already have:

```sh
git config --global gpg.ssh.program ~/.local/bin/git-ssh-sign
git config --global user.signingkey "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAACAQCdemZz7dZLzntCMvrefRlIe2SCoGoU/wKMc/IFx6FvlIr9cdCl9q+CkaaJ0n3mvmDhVHkWYCWfe8F6P3jjOAy+w+pp6q7yoBFX88rGSafRoP2VUuaqGt+JWYiGY3hHVHzmFXqqSW4dqMZEM7raO6ui9iRet3nqT2CQqOvw3bdO18+iEEjQ2MSAFIKgtoEB28QQPiJeSTmJGie6ry+/UOa9YOQRQlII3SQ0cqxDROLTI1Cjy82epDzinBN5zNeC1Fe0XThaT238OjegNe8C0oOaE0GcT3R3NAyYWzVAvL61fjT5bEkN9Y8/nQIk1yZ1teGd+oW8izP4OUpAC6WfP3DNVv2kmvvwTAPo1fjKcu4jvXg2siMOlImsrr7fLqE83gqm6BSZuMQ0AybwHIht869c1k7De/obxfZFzfzj/Ld/EfTE1/uWcdebiepKJ8z50HLSMyxz+8HZHtCclC1HkP30wbLkbURPnw8Xx8xlAatSZjUHtJLA53lQQM/+TXWx3sz2ZpRIZXs7fnwWZqIgwTPRQAELz4WhLy0adzDTLMh4nVaVcn61nH1p/EKHAQ34Zl1uoPtXTVHUT2nYi8Lh95gODrcM4/eyUfKEPYitRtWWjJhnaXLKlLsK1oeon+hYyPmgnXMRTMY8JCmUBuuIRpwYo+zNY07M2SkXV58RJsVorw== docs-example"
```

`trg git ssh sign` takes the arguments git gives `ssh-keygen`. Every `-Y` mode
other than `sign`, such as `verify` and `find-principals`, is run by the real
`ssh-keygen` with the same arguments, streams and exit status. The `-U` flag git
adds for a literal key is accepted, and the progress lines `ssh-keygen` prints
are printed too.

## How long it waits

An OpenBao backend gives up on a request after its `timeout_ms`, which is
`5000` unless the backend sets it, and a connection after at most `2000`. A
Transit that stops answering therefore costs one timeout before the next
address is tried, and the warning says it did not answer in time. Lower
`timeout_ms` on the backend if the fallback should take over sooner.

## What happens when a signer fails

An address falls through to the next only when it could not be asked, never
when it refused. A person who denies an approval prompt must not have the
request signed by another key holder behind their back.

| Address | Outcome | Next |
|---|---|---|
| Transit | Backend unreachable, timeout, or a transport error | Falls through |
| Transit | HTTP 403, so the token was rejected | Falls through |
| Transit | Key missing, not an `ed25519` or RSA key, or its public key differs from `public_key` | Stops |
| Transit | Malformed response | Stops |
| Transit | The signature does not verify against `public_key` | Stops, and no `.sig` is written |
| SSH agent | Socket missing or connection refused | Falls through |
| SSH agent | Key not listed by the agent | Falls through |
| SSH agent | Signing fails after the key was listed, such as a denied prompt | Stops |

`trg` checks every Transit signature against `public_key` before it writes
`<file>.sig`, so a signature that was damaged or made by another key never
reaches git. This is not retried on another address, because a signer that
returns a wrong answer should be looked at, not routed around.

When a fallback signs, `trg` prints a warning on stderr that names the signer,
why the earlier address failed, and which address answered.

See the [config reference](../reference/config.md#gitsshsignersname) for every
field.
