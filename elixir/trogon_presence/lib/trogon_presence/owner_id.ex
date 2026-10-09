defmodule TrogonPresence.OwnerId do
  @moduledoc """
  Identifies the service instance that owns a writer or view shard lease.
  Matches `trogon_presence::position::OwnerId`. The shim only ever parses
  this id out of wire headers; it never mints one.
  """

  use TrogonPresence.OpaqueId
end
