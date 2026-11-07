# shellcheck shell=bash
lints_toolchain() {
  sed -n 's/^channel = "\(.*\)"/\1/p' "$MISE_PROJECT_ROOT/crates/trogon_dylint_lints/rust-toolchain.toml"
}

lints_exec() {
  local channel dylint_dir linker_dir
  channel="$(lints_toolchain)"
  dylint_dir="$(mise where cargo:cargo-dylint)" || return
  linker_dir="$(mise where cargo:dylint-link)" || return
  mise exec rust@"$channel" -- /bin/sh -c '
    export PATH="$1:$PATH"
    shift
    exec "$@"
  ' lints-exec "$dylint_dir/bin:$linker_dir/bin" "$@"
}

lints_cargo() {
  lints_exec cargo "$@"
}

lints_library() {
  local channel host extension
  channel="$(lints_toolchain)"
  host="$(rustup run "$channel" rustc -vV | sed -n 's/^host: //p')"
  case "$host" in
    *-apple-*) extension=dylib ;;
    *-linux-*) extension=so ;;
    *) printf 'Unsupported policy lint host: %s\n' "$host" >&2; return 1 ;;
  esac
  printf '%s/crates/trogon_dylint_lints/target/release/libtrogon_dylint_lints@%s-%s.%s\n' "$MISE_PROJECT_ROOT" "$channel" "$host" "$extension"
}
