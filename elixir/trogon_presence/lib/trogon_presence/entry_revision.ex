defmodule TrogonPresence.EntryRevision do
  @moduledoc """
  The revision a shard lease was acquired at, carried in the
  `Presence-Owner-Rev` header as part of a `GenerationEpoch`. Matches
  `trogon_presence::position::EntryRevision`.
  """

  use TrogonPresence.DecimalSequence
end
