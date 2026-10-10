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

  @doc """
  The diff that turns `previous` into `current`, matching metas by
  `phx_ref`: a meta only in `current` joins and a meta only in `previous`
  leaves, so an update shows up as the leave of its old `phx_ref` and the
  join of its new one, the way `Phoenix.Presence` reports it.
  """
  @spec between(Presences.t(), Presences.t()) :: t()
  def between(previous, current) when is_map(previous) and is_map(current) do
    %__MODULE__{joins: missing_from(current, previous), leaves: missing_from(previous, current)}
  end

  @spec empty?(t()) :: boolean()
  def empty?(%__MODULE__{joins: joins, leaves: leaves}), do: joins == %{} and leaves == %{}

  defp missing_from(source, other) do
    source
    |> Map.new(fn {key, metas} ->
      present = other |> Map.get(key, []) |> MapSet.new(& &1.phx_ref)
      {key, Enum.reject(metas, &MapSet.member?(present, &1.phx_ref))}
    end)
    |> Presences.from_map()
  end

  @spec encode(t()) :: map()
  def encode(%__MODULE__{joins: joins, leaves: leaves}) do
    %{"joins" => Presences.encode(joins), "leaves" => Presences.encode(leaves)}
  end
end
