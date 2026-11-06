# Contributing

## Setup

```bash
git clone https://github.com/TrogonStack/rusty-monorepo.git
cd rusty-monorepo
mise install rust
mise run build:debug
```

Tasks use explicitly installed tools. Install optional tools with `mise install`
only when needed, for example `mise install cargo:cargo-audit` before a dependency audit.

## Before Submitting

Run the verification suite:

```bash
mise run check
```

This runs workspace formatting, strict Clippy, unit/integration and documentation
tests. All must pass before merge.

Task implementations live under `.config/mise/tasks/`, following
[ADR 6647967335](https://github.com/straw-hat-team/adr/blob/af44966a0b1fd8422ab3749013a5e3dcac622e02/src/adrs/6647967335/README.md).

The workspace Clippy policy, split across `Cargo.toml` and `clippy.toml`, mirrors `policies/rust/`, the reference copy shared with other repositories. Change them together.

## Licensing

New Rust crates use Apache-2.0. Inherit the workspace license in the package
manifest:

```toml
[package]
license.workspace = true
```

Include the full Apache-2.0 text in the crate's `LICENSE`, including its copyright
notice. The existing [lint crate license](crates/trogon_dylint_lints/LICENSE) is a
template. Preserve required third-party notices when incorporating existing code.

Existing package licenses remain as declared in their manifests and local license
files. The root MIT license applies to repository files without a more specific
license; it is not the default for new Rust crates.

## Commit Messages

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <subject>
```

Types: `feat`, `fix`, `chore`
