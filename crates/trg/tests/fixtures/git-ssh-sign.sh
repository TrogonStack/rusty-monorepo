#!/bin/sh
# The pass-through file a user points `gpg.ssh.program` at, with the `trg`
# under test named by `TRG_BIN`.
#
# It is checked in rather than written at test time because a file this process
# has just written may still be open for writing in a child it forked for some
# other test, and executing such a file fails with ETXTBSY.
exec "$TRG_BIN" git ssh sign "$@"
