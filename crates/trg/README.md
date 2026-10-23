# trg

> **Platform support:** Linux and macOS only. Windows is not supported — use [WSL](https://learn.microsoft.com/windows/wsl/) (clone and build inside the WSL filesystem for full functionality).

## Installation

`trg` binaries are published to the GitHub releases of [TrogonStack/rusty-monorepo](https://github.com/TrogonStack/rusty-monorepo/releases). Because the repository also publishes releases for other crates, `version_prefix` tells mise to only consider `trg@v*` tags.

Install it globally:

```sh
mise use -g "github:TrogonStack/rusty-monorepo[version_prefix=trg@v]@latest"
```

Or pin it in a project's `mise.toml`:

```toml
[tools]
"github:TrogonStack/rusty-monorepo" = { version = "0.11.0", version_prefix = "trg@v" }
```

Then verify the installation:

```sh
trg --version
```

## Documentation

See the [documentation index](docs/README.md).
