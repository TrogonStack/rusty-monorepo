defmodule TrogonPresence.Presences do
  @moduledoc """
  A reader's view of every tracked key on a topic. Matches
  `trogon_presence::watch::presences::Presences`: `%{"<key>" => %{"metas" =>
  [...]}}`, the same shape `Phoenix.Presence.list/1` returns.
  """

  alias TrogonPresence.{Key, MetaEntry}

  @type t :: %{Key.t() => [MetaEntry.t()]}

  @spec decode(map()) :: {:ok, t()} | :error
  def decode(wire) when is_map(wire) do
    Enum.reduce_while(wire, {:ok, %{}}, fn {raw_key, entry}, {:ok, acc} ->
      with {:ok, key} <- Key.new(raw_key),
           {:ok, metas} <- decode_metas(entry) do
        {:cont, {:ok, Map.put(acc, key, metas)}}
      else
        _ -> {:halt, :error}
      end
    end)
  end

  def decode(_wire), do: :error

  defp decode_metas(%{"metas" => metas}) when is_list(metas) do
    Enum.reduce_while(metas, {:ok, []}, fn wire, {:ok, acc} ->
      case MetaEntry.decode(wire) do
        {:ok, entry} -> {:cont, {:ok, [entry | acc]}}
        :error -> {:halt, :error}
      end
    end)
    |> case do
      {:ok, reversed} -> {:ok, Enum.reverse(reversed)}
      :error -> :error
    end
  end

  defp decode_metas(_entry), do: :error

  @doc "Drops keys whose meta list is empty, matching `Presences::from`."
  @spec from_map(%{Key.t() => [MetaEntry.t()]}) :: t()
  def from_map(map) when is_map(map) do
    map |> Enum.reject(fn {_key, metas} -> metas == [] end) |> Map.new()
  end

  @spec encode(t()) :: map()
  def encode(presences) when is_map(presences) do
    Map.new(presences, fn {key, metas} ->
      {Key.raw(key), %{"metas" => Enum.map(metas, &MetaEntry.encode/1)}}
    end)
  end

  @doc """
  Merges a diff's joins and leaves into `state`. Mirrors
  `trogon_presence_service::reader::sync_diff`: a join replaces any existing
  metas sharing its `phx_ref`s and appends the rest; a leave drops metas
  matching its `phx_ref`s and removes the key entirely once empty.
  """
  @spec apply_diff(t(), TrogonPresence.Diff.t()) :: t()
  def apply_diff(state, %TrogonPresence.Diff{joins: joins, leaves: leaves}) do
    state |> apply_joins(joins) |> apply_leaves(leaves)
  end

  defp apply_joins(state, joins) do
    Enum.reduce(joins, state, fn {key, joined}, state ->
      joined_refs = MapSet.new(joined, & &1.phx_ref)

      existing =
        state |> Map.get(key, []) |> Enum.reject(&MapSet.member?(joined_refs, &1.phx_ref))

      Map.put(state, key, existing ++ joined)
    end)
  end

  defp apply_leaves(state, leaves) do
    Enum.reduce(leaves, state, fn {key, left}, state ->
      case Map.fetch(state, key) do
        :error ->
          state

        {:ok, metas} ->
          left_refs = MapSet.new(left, & &1.phx_ref)
          remaining = Enum.reject(metas, &MapSet.member?(left_refs, &1.phx_ref))
          if remaining == [], do: Map.delete(state, key), else: Map.put(state, key, remaining)
      end
    end)
  end
end
