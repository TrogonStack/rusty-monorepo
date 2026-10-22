defmodule TrogonPresence.LocalViewId do
  @moduledoc """
  Disambiguates one reader instance's reply inbox from any other reader the
  same connection may start for the same topic. Matches
  `trogon_presence::position::LocalViewId`.
  """

  use TrogonPresence.OpaqueId
end
