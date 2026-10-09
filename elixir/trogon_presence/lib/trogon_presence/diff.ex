defmodule TrogonPresence.Diff do
  @moduledoc """
  A join/leave diff between two snapshots of a topic. Matches
  `trogon_presence::watch::presences::Diff`.
  """

  alias TrogonPresence.Presences

  @enforce_keys [:joins, :leaves]
  defstruct [:joins, :leaves]
  @type t :: %__MODULE__{joins: Presences.t(), leaves: Presences.t()}

  @spec decode(map()) :: {:ok, t()} | :error
  def decode(%{"joins" => joins, "leaves" => leaves} = wire) when map_size(wire) == 2 do
    with {:ok, joins} <- Presences.decode(joins),
         {:ok, leaves} <- Presences.decode(leaves) do
      {:ok, %__MODULE__{joins: joins, leaves: leaves}}
    else
      _ -> :error
    end
  end

  def decode(_wire), do: :error

  @spec encode(t()) :: map()
  def encode(%__MODULE__{joins: joins, leaves: leaves}) do
    %{"joins" => Presences.encode(joins), "leaves" => Presences.encode(leaves)}
  end
end
