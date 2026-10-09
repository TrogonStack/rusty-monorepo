defmodule TrogonPresence.Test.NatsReadPresence do
  @moduledoc """
  A `TrogonPresence` module pinned to `backend: :nats_read`: writes still go
  to both Phoenix and NATS, but reads, diffs and the ref returned from
  `track/4`/`update/4` now come from NATS. `:conn` is a placeholder at
  compile time, overridden per test the same way `TrogonPresence.Test.Presence`
  is.
  """

  use TrogonPresence,
    backend: :nats_read,
    conn: nil,
    pubsub_server: TrogonPresence.Test.PubSub
end
