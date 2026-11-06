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
tests, and the claims dependency, feature-boundary and documentation checks. All
must pass before merge.

For a focused claims change, run `mise run claims:check`. Task implementations
live under `.config/mise/tasks/`, following
[ADR 6647967335](https://github.com/straw-hat-team/adr/blob/af44966a0b1fd8422ab3749013a5e3dcac622e02/src/adrs/6647967335/README.md).

## Commit Messages

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <subject>
```

Types: `feat`, `fix`, `chore`
