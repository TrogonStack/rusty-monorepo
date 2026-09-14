#!/bin/sh
# A stand-in for the `op` binary, linked into a temporary directory as `op` and
# paired with a `op.body` script the test writes.
#
# It is checked in rather than written at test time because a file this process
# has just written is a file this process may still hold a write handle to in a
# child it forked for some other test, and executing such a file fails with
# ETXTBSY. Reading a body script is not executing it, so only this file needs to
# be executable, and nothing ever writes it.
for a in "$@"; do
  printf '%s\n' "$a" >> "${0}.argv"
done
exec /bin/sh "${0}.body" "$@"
