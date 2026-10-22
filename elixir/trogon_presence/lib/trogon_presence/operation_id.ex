defmodule TrogonPresence.OperationId do
  @moduledoc """
  Identifies one write request for idempotent retry, minted once per logical
  track/update/untrack/release call. Matches
  `trogon_presence::position::OperationId`.
  """

  use TrogonPresence.OpaqueId
end
