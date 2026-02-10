# Contributing

## Setup

```bash
git clone https://github.com/TrogonStack/rusty-monorepo.git
cd rusty-monorepo
cargo build --workspace
```

## Before Submitting

Run the verification suite:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All must pass before merge.

## Commit Messages

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <subject>
```

Types: `feat`, `fix`, `chore`
