defmodule TrogonPresence.Test.RealPresence do
  @moduledoc """
  A real `Phoenix.Presence` module, started alongside the shim so the parity
  test can run the same operation sequence through both and compare results.
  """

  use Phoenix.Presence,
    otp_app: :trogon_presence,
    pubsub_server: TrogonPresence.Test.PubSub
end
