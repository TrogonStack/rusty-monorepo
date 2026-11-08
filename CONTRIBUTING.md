# Contributing

## Setup

```bash
git clone https://github.com/TrogonStack/rusty-monorepo.git
cd rusty-monorepo
mise run setup
```

Setup installs Rust, the shared policy lint tools and their pinned nightly, then
builds the workspace. Install optional tools with `mise install` only when needed,
for example `mise install cargo:cargo-audit` before a dependency audit.

## Before Submitting

Run the verification suite:

```bash
mise run check
```

This runs workspace formatting, strict Clippy, unit/integration and documentation
tests, and shared policy lints across all features and targets. All must pass
before merge.

Task implementations live under `.config/mise/tasks/`, following
[ADR 6647967335](https://github.com/straw-hat-team/adr/blob/af44966a0b1fd8422ab3749013a5e3dcac622e02/src/adrs/6647967335/README.md).

The workspace Clippy policy, split across `Cargo.toml` and `clippy.toml`, mirrors `policies/rust/`, the reference copy shared with other repositories. Change them together.

Policy checks build the lint library from this checkout using its pinned nightly:

```bash
mise run lints:run
```

The workspace selects the portable profile in `dylint.toml` because `trg` owns
its environment and telemetry adapters. Structural and safety rules remain
active. Keep deliberate exceptions local and state their technical reason in
`cfg_attr(dylint_lib = "trogon_dylint_lints", expect(rule, reason = "..."))`.
CI runs the workspace checks alongside the lint library's UI tests.

## Commit Messages

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <subject>
```

Types: `feat`, `fix`, `chore`
