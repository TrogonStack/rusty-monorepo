defmodule TrogonPresence.ConnectionId do
  @moduledoc """
  Identifies one reader connection. Scopes a reader's reply inbox and its
  snapshot-reply subscription so the service can address it directly.
  Matches `trogon_presence::position::ConnectionId`.
  """

  use TrogonPresence.OpaqueId
end
