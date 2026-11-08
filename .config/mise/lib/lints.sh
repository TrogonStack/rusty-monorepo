# shellcheck shell=bash
lints_toolchain() {
  sed -n 's/^channel = "\(.*\)"/\1/p' "$MISE_PROJECT_ROOT/crates/trogon_dylint_lints/rust-toolchain.toml"
}

lints_rustup_proxy_dir() {
  local rustup_path proxy_dir
  rustup_path="$(mise which rustup)" || return
  proxy_dir="$(dirname "$rustup_path")"
  test -x "$proxy_dir/cargo" || return
  printf '%s\n' "$proxy_dir"
}

lints_exec() {
  local channel dylint_dir linker_dir cargo_path proxy_dir
  channel="$(lints_toolchain)"
  dylint_dir="$(mise where cargo:cargo-dylint)" || return
  linker_dir="$(mise where cargo:dylint-link)" || return
  cargo_path="$(rustup which --toolchain "$channel" cargo)" || return
  proxy_dir="$(lints_rustup_proxy_dir)" || return
  mise exec rust@"$channel" -- /bin/sh -c '
    lint_path=":$PATH:"
    # Rustup must restore the toolchain after Dylint sanitizes its environment.
    case "$lint_path" in
      *":$3:"*)
        lint_prefix="${lint_path%%":$3:"*}"
        lint_suffix="${lint_path#*":$3:"}"
        lint_path="$lint_prefix:$4:$lint_suffix"
        ;;
      *) lint_path="$lint_path$4:" ;;
    esac
    lint_path="${lint_path#:}"
    lint_path="${lint_path%:}"
    export PATH="$1:$lint_path"
    export RUSTUP_TOOLCHAIN="$2"
    shift 4
    exec "$@"
  ' lints-exec "$dylint_dir/bin:$linker_dir/bin" "$channel" "$(dirname "$cargo_path")" "$proxy_dir" "$@"
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
