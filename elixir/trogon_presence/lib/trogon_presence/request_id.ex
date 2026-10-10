defmodule TrogonPresence.RequestId do
  @moduledoc """
  Identifies one snapshot request, minted by the reader and echoed back in
  the manifest and every frame of that snapshot. Matches
  `trogon_presence::position::RequestId`.
  """

  use TrogonPresence.OpaqueId
end
