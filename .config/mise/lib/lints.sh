# shellcheck shell=bash
lints_toolchain() {
  sed -n 's/^channel = "\(.*\)"/\1/p' "$MISE_PROJECT_ROOT/crates/trogon_dylint_lints/rust-toolchain.toml"
}
