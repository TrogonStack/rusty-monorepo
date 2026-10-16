defmodule TrogonPresence.HolderId do
  @moduledoc """
  Identifies the node-local process that is holding one or more tracked
  entries, so a single `release`/`heartbeat` batch can cover every topic that
  process is keeping alive. Matches `trogon_presence::holder::HolderId`.
  """

  use TrogonPresence.OpaqueId
end
