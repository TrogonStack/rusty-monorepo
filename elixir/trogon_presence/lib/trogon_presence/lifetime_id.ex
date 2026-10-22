defmodule TrogonPresence.LifetimeId do
  @moduledoc """
  Identifies one join's lifetime on the server, returned from a successful
  `track` and required on every later `update`/`untrack` of that entry.
  Matches `trogon_presence::position::LifetimeId`.
  """

  use TrogonPresence.OpaqueId
end
