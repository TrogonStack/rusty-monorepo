#!/usr/bin/env bash
set -euo pipefail
source "${MISE_PROJECT_ROOT:?}/.config/mise/lib/lints.sh"
channel="$(lints_toolchain)"
cargo_path="$(rustup which --toolchain "$channel" cargo)"
toolchain_lib="$(dirname "$(dirname "$cargo_path")")/lib"
host="$(rustup run "$channel" rustc -vV | sed -n 's/^host: //p')"
binary_dir="$(dirname "${1:?missing test binary}")"
runtime_paths="$toolchain_lib:$toolchain_lib/rustlib/$host/lib:$binary_dir"
# The UI harness parses raw rustc commands. Keep the outer managed Cargo lease,
# while its test subprocesses use the pinned compiler without cache wrappers.
export PATH="$(dirname "$cargo_path"):$PATH"
export DYLD_LIBRARY_PATH="$runtime_paths${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}"
export LD_LIBRARY_PATH="$runtime_paths${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
unset CARGO RUSTC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER
exec "$@"
