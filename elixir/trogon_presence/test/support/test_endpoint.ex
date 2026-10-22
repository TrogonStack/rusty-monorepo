defmodule TrogonPresence.Test.Endpoint do
  @moduledoc """
  The smallest `Phoenix.Endpoint` that can carry a channel socket under
  `Phoenix.ChannelTest`: no HTTP server, no router, no `config/*.exs` file.
  `Phoenix.Endpoint.Supervisor` merges whatever opts a caller passes to
  `start_supervised!({TrogonPresence.Test.Endpoint, opts})` straight into its
  config (see `defaults/2` and `init/1` in `phoenix/endpoint/supervisor.ex`),
  so the test setup supplies `:pubsub_server`, `:secret_key_base` and
  `server: false` that way instead of through a deprecated `init/2` callback.
  """

  use Phoenix.Endpoint, otp_app: :trogon_presence

  socket("/socket", TrogonPresence.Test.UserSocket, websocket: true)
end
