defmodule TrogonPresence.SnapshotId do
  @moduledoc """
  Identifies one captured snapshot. Matches
  `trogon_presence::position::SnapshotId`. The shim only ever parses this id
  out of a snapshot manifest; it never mints one.
  """

  use TrogonPresence.OpaqueId
end
