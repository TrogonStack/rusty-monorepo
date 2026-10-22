defmodule TrogonPresence.MutationSequence do
  @moduledoc """
  The per-entry mutation counter returned by track/update and required by
  the next update/untrack of that entry. Matches
  `trogon_presence::position::MutationSequence`.
  """

  use TrogonPresence.DecimalSequence
end
