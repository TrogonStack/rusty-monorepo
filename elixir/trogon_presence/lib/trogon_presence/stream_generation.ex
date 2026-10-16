defmodule TrogonPresence.StreamGeneration do
  @moduledoc """
  Identifies one generation of the presence service's backing stream.
  Matches `trogon_presence::position::StreamGeneration`. The shim only ever
  parses this id out of wire headers/bodies; it never mints one.
  """

  use TrogonPresence.OpaqueId
end
