<div align="center">
  <img src="./logo.png" alt="Logo" width="300"> 
</div>

<div align="center">

# Rusty MonoRepo

A Rust-based monorepo for building multiple projects.

</div>

## Install `trg` with mise

`trg` binaries are published to this repository's GitHub releases. Because the repository also publishes releases for other crates, `version_prefix` tells mise to only consider `trg@v*` tags.

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
