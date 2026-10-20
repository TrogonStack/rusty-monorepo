#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"

base="thiserror trogon_claims_macros trogon_claims_syntax zeroize"

allowed() {
  case "$1" in
    "") echo "$base" ;;
    serde) echo "$base serde" ;;
    json) echo "$base serde_json" ;;
    spicedb) echo "$base prost prost-types" ;;
    axum) echo "$base axum-core http" ;;
  esac
}

direct_dependencies() {
  cargo tree -p trogon_claims -e no-dev --depth 1 --prefix none --no-default-features ${1:+--features "$1"} \
    | tail -n +2 | awk '{print $1}' | sort -u | tr '\n' ' '
}

status=0
for feature in "" serde json spicedb axum; do
  actual="$(direct_dependencies "$feature")"
  expected="$(allowed "$feature" | tr ' ' '\n' | sort -u | tr '\n' ' ')"
  if [ "$actual" != "$expected" ]; then
    echo "dependency budget violated for feature '${feature:-default}'" >&2
    echo "  expected: $expected" >&2
    echo "  actual:   $actual" >&2
    status=1
  fi
done

if cargo tree -p trogon_claims --all-features -e no-dev --prefix none | awk '{print $1}' | grep -qx secrecy; then
  echo "secrecy must not appear in the trogon_claims dependency tree" >&2
  status=1
fi

syntax_deps="$(cargo tree -p trogon_claims_syntax -e no-dev --depth 1 --prefix none | tail -n +2 | wc -l | tr -d ' ')"
if [ "$syntax_deps" != "0" ]; then
  echo "trogon_claims_syntax must have no dependencies" >&2
  status=1
fi

goldens=(crates/trogon_claims/tests/ui crates/trogon_claims/tests/ui_spicedb)

if grep -rlE '/Users|/Volumes|/home|C:\\' "${goldens[@]}"; then
  echo "absolute paths found in trybuild goldens" >&2
  status=1
fi

if grep -rlF '$RUST' "${goldens[@]}"; then
  echo "trybuild goldens quote standard library source, which depends on rust-src being installed" >&2
  status=1
fi

exit "$status"
