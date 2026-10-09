defmodule TrogonPresence.Test.PhoenixPresence do
  @moduledoc """
  A `TrogonPresence` module pinned to `backend: :phoenix`: the stage that
  delegates fully to a real `Phoenix.Presence`, used to prove a host app can
  adopt `TrogonPresence` before anything behavioral changes.
  """

  use TrogonPresence,
    backend: :phoenix,
    pubsub_server: TrogonPresence.Test.PubSub
end
