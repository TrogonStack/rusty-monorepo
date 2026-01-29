# Justfile - command runner for development tasks
# Install: https://github.com/casey/just
# Usage: just <command>

set shell := ["bash", "-c"]

# Default: show help
default:
    @just --list

# Format code
fmt:
    cargo fmt --all

# Check formatting without modifying
fmt-check:
    cargo fmt --all -- --check

# Run clippy with strict warnings
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Run all tests
test:
    cargo test --workspace

# Run tests with output
test-verbose:
    cargo test --workspace -- --nocapture

# Run doc tests
test-doc:
    cargo test --workspace --doc

# Build debug artifacts
build:
    cargo build --workspace

# Build release artifacts
build-release:
    cargo build --workspace --release

# Run verification suite (fmt check + lint + test)
verify:
    #!/bin/bash
    set -e
    echo "🔍 Checking formatting..."
    cargo fmt --all -- --check
    echo "✓ Format check passed"

    echo "🔍 Running clippy..."
    cargo clippy --workspace --all-targets -- -D warnings
    echo "✓ Clippy check passed"

    echo "🔍 Running tests..."
    cargo test --workspace
    echo "✓ Tests passed"

    echo "🚀 All verification checks passed!"

# Run security audit
audit:
    cargo audit

# Fix formatting issues automatically
fix:
    cargo fmt --all
    cargo clippy --workspace --fix --allow-dirty --allow-staged

# Clean build artifacts
clean:
    cargo clean

# Check code without building
check:
    cargo check --workspace

# Generate documentation
doc:
    cargo doc --workspace --no-deps --open

# Run all CI checks locally (mimics GitHub Actions)
ci: verify audit
    @echo "✅ All CI checks passed!"

# Display project info
info:
    @echo "Project: trogontools (Rust)"
    @echo "Workspace members:"
    @cargo metadata --format-version 1 | jq '.workspace_members[]' -r | sed 's/ .*//' | sed 's/^/  - /'
    @echo ""
    @echo "Rust version:"
    @rustc --version
    @echo "Cargo version:"
    @cargo --version

# Watch for changes and run tests
watch-test:
    @command -v cargo-watch >/dev/null 2>&1 || cargo install cargo-watch
    cargo watch -x test

# Profile binary sizes
bloat:
    @command -v cargo-bloat >/dev/null 2>&1 || cargo install cargo-bloat
    cargo bloat --workspace --release
