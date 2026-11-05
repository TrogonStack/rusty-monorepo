# trg

> **Platform support:** Linux and macOS only. Windows is not supported — use [WSL](https://learn.microsoft.com/windows/wsl/) (clone and build inside the WSL filesystem for full functionality).

## Installation

`trg` binaries are published to the GitHub releases of [TrogonStack/rusty-monorepo](https://github.com/TrogonStack/rusty-monorepo/releases). Because the repository also publishes releases for other crates, `version_prefix` tells mise to only consider `trg@v*` tags.

Install it globally:

```sh
mise use -g "github:TrogonStack/rusty-monorepo[version_prefix=trg@v]@latest"
```

Or pin it in a project's `mise.toml`:

```toml trg-example=skip
[tools]
"github:TrogonStack/rusty-monorepo" = { version = "0.11.0", version_prefix = "trg@v" }
```

Then verify the installation:

```sh
trg --version
```

## Verifying a download

Every release tarball and `sha256sums.txt` carries a signed build provenance attestation. Pin both the repository and the signing workflow, otherwise the check only proves that some GitHub workflow built the file:

```sh trg-example=skip
gh attestation verify trg-<version>-<target>.tar.gz \
  --repo TrogonStack/rusty-monorepo \
  --signer-workflow TrogonStack/rusty-monorepo/.github/workflows/release-artifacts.yml
```

Each release also ships `packslip.sigstore.json`, a signed manifest of every artifact, and a SLSA provenance file (`*.intoto.jsonl`) that can be checked with [slsa-verifier](https://github.com/slsa-framework/slsa-verifier).

## Documentation

See the [documentation index](docs/README.md).
