# shellcheck shell=bash
object_id_forbidden_features() {
  grep -E 'features=\[[^]]*(std|alloc|rng|getrandom|v4|v7)|getrandom v'
}
