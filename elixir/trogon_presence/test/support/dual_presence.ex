defmodule TrogonPresence.Test.DualPresence do
  @moduledoc """
  A `TrogonPresence` module pinned to `backend: :dual`: writes go to a real
  `Phoenix.Presence` first and are mirrored, best-effort, to NATS; reads and
  broadcasts stay on Phoenix. `:conn` is a placeholder at compile time,
  overridden per test the same way `TrogonPresence.Test.Presence` is.
  """

  use TrogonPresence,
    backend: :dual,
    conn: nil,
    pubsub_server: TrogonPresence.Test.PubSub
end
