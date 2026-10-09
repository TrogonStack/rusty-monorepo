defmodule TrogonPresence.DiffSequence do
  @moduledoc """
  The per-view diff counter carried on every diff/keepalive frame and
  snapshot identity, used to detect gaps and replays. Matches
  `trogon_presence::position::DiffSequence`.
  """

  use TrogonPresence.DecimalSequence
end
