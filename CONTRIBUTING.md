# Contributing to trogontools

Thank you for your interest in contributing! This document outlines the development process and quality standards.

## Development Setup

### Prerequisites

- Rust 1.93.0 or later (install via [rustup](https://rustup.rs/))
- `cargo` (installed with Rust)
- Optional: `just` for convenient command running

### Quick Start

```bash
# Clone the repository
git clone https://github.com/TrogonStack/rusty-monorepo.git
cd rusty-monorepo

# Verify your setup works
cargo build --workspace
cargo test --workspace
```

## Quality Standards

All pull requests must pass the following checks:

### 1. Formatting (`rustfmt`)

Code must be formatted according to Rust standards.

```bash
# Check formatting
cargo fmt --all -- --check

# Auto-fix formatting issues
cargo fmt --all
```

**Configuration**: See `.rustfmt.toml`

### 2. Linting (`clippy`)

All clippy lints with `-D warnings` must pass (strict mode).

```bash
# Run clippy checks
cargo clippy --workspace --all-targets -- -D warnings

# Or use the alias
cargo lint
```

**Why strict mode?**
- Catches bugs early
- Prevents code smell
- Ensures consistency
- Enforces best practices

### 3. Testing

All tests must pass on your branch before submission.

```bash
# Run all tests
cargo test --workspace

# Run specific test
cargo test --lib parser::tests

# Run with output
cargo test --workspace -- --nocapture

# Run doc tests
cargo test --doc
```

### 4. Security Audit

Check for known vulnerabilities in dependencies.

```bash
cargo audit
```

### 5. Documentation

- Add doc comments to public items
- Keep comments focused on "why", not "what"
- Run `cargo doc --no-deps` to preview

## Pre-Commit Verification

Before pushing, run the full verification suite:

```bash
# Option 1: Using just
just verify

# Option 2: Manual commands
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo audit
```

## Making Changes

### Step 1: Create a Feature Branch

```bash
git checkout -b feature/your-feature-name
```

### Step 2: Make Your Changes

Follow these principles:

- **Small focused commits**: One logical change per commit
- **Meaningful commit messages**: Use conventional commits (feat:, fix:, refactor:, etc.)
- **No reformatting of unrelated code**: Keep changes focused
- **Add tests**: New functionality needs tests

### Step 3: Run Local Checks

```bash
just verify
```

### Step 4: Push and Open PR

```bash
git push origin feature/your-feature-name
```

GitHub will automatically run CI checks. All must pass before merge.

## Commit Message Format

Follow the [Conventional Commits](https://www.conventionalcommits.org/) specification:

```
<type>(<scope>): <subject>

<body>

<footer>
```

### Types

- `feat`: New feature
- `fix`: Bug fix
- `refactor`: Code refactoring (no feature change)
- `perf`: Performance improvement
- `test`: Add/update tests
- `docs`: Documentation only
- `chore`: Dependency updates, configuration

### Examples

```
feat(agentskills): add field validation for frontmatter

Validates that only allowed fields are present in SKILL.md frontmatter,
catching typos and preventing silent failures.

Fixes #42
```

```
fix(parser): handle missing required fields correctly
```

```
docs: update README with examples
```

## Architecture

### Workspace Structure

```
rusty-monorepo/
├── crates/
│   ├── trogontools/          # CLI binary
│   └── agentskills/          # Core library
└── .github/workflows/         # CI configuration
```

### Key Design Principles

- **Separation of concerns**: Library logic separate from CLI
- **Testability**: All code uses pluggable dependencies (e.g., FileSystem trait)
- **In-memory testing**: Tests use MemFS, not real disk I/O
- **Error handling**: Comprehensive error types with context

## Testing Guidelines

### Unit Tests

Place tests in the same module as the code they test:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_specific_behavior() {
        // Arrange
        let input = "test";

        // Act
        let result = function_under_test(input);

        // Assert
        assert_eq!(result, expected);
    }
}
```

### Integration Tests

For cross-module scenarios, use in-memory MemFS:

```rust
#[test]
fn test_full_workflow() {
    let fs = MemFS::new();
    fs.insert(Path::new("/skill/SKILL.md"), "---\nname: skill\n---");

    let props = read_properties(&fs, Path::new("/skill")).unwrap();
    assert_eq!(props.name, "skill");
}
```

### Test Coverage

- Aim for >80% code coverage
- Cover error paths
- Test edge cases
- Use meaningful assertions

## Code Review Process

1. **Automated checks**: GitHub Actions must pass
2. **Code review**: At least one maintainer reviews
3. **Approval**: Review approval before merge
4. **Merge**: Squash or rebase as appropriate

### What Reviewers Look For

- ✅ Follows quality standards (fmt, clippy, tests)
- ✅ Solves the problem clearly
- ✅ Doesn't introduce unnecessary complexity
- ✅ Has tests for new functionality
- ✅ Maintains backward compatibility
- ✅ Clear commit messages

## Common Mistakes to Avoid

❌ **Mixing unrelated changes**: Keep PR focused on one feature

❌ **Ignoring clippy warnings**: Fix warnings, don't suppress them

❌ **Skipping tests**: All new code needs tests

❌ **Reformatting unrelated code**: Run `cargo fmt --all` on whole codebase separately

❌ **Large commits**: Break into logical, reviewable pieces

❌ **Poor commit messages**: Use conventional commits

## Debugging

### Run tests with backtrace

```bash
RUST_BACKTRACE=1 cargo test --workspace -- --nocapture
```

### Run single test

```bash
cargo test module::test_name -- --nocapture
```

### Profile binary size

```bash
cargo install cargo-bloat
cargo bloat --workspace --release
```

## Performance Considerations

- Keep test execution fast (< 1s for unit tests)
- Use `.internal.trogonai.md` suffix for internal documentation (excluded from git)
- Cache build artifacts in CI
- Profile before optimizing

## Documentation

### Doc Comments

```rust
/// Brief description of the item.
///
/// More detailed explanation if needed.
///
/// # Examples
///
/// ```
/// let result = function(arg);
/// assert_eq!(result, expected);
/// ```
pub fn function(arg: &str) -> String {
    // implementation
}
```

### Inline Comments

Only use comments to explain "why", not "what":

```rust
// ✅ Good - explains why
let mut errors = Vec::new();
// Collect all errors before returning to show user multiple issues at once
for validator in validators {
    if let Err(e) = validator() {
        errors.push(e);
    }
}

// ❌ Bad - just repeats what the code does
let mut errors = Vec::new();  // create a vector
errors.push(e);               // push the error
```

## Useful Commands

```bash
# Development
just verify          # Run full verification suite
just test           # Run tests
just fmt            # Format code
just lint           # Run clippy
just build          # Build debug binary
just build-release  # Build release binary
just clean          # Clean build artifacts
just fix            # Auto-fix formatting + clippy suggestions

# Analysis
just doc            # Generate and open documentation
just audit          # Security audit
just info           # Show project info
just bloat          # Analyze binary size

# Continuous Development
just watch-test     # Watch and run tests on file changes
```

## Getting Help

- **GitHub Issues**: For bugs or feature requests
- **GitHub Discussions**: For questions
- **Documentation**: Check `.trogonai/` directory for detailed guides

## Code of Conduct

- Be respectful and inclusive
- Assume good intent
- Provide constructive feedback
- Report issues through proper channels

---

Thank you for contributing to trogontools! 🚀
