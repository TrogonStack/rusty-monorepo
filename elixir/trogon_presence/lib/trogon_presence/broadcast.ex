defmodule TrogonPresence.Broadcast do
  @moduledoc """
  Shapes a `TrogonPresence.Presences`/`TrogonPresence.Diff` into exactly the
  wire format `Phoenix.Presence` itself produces, so a channel client cannot
  tell the backend apart from the stock ETS-backed one: a topic's presences
  as `%{key => %{metas: [...]}}`, and a diff as `%{joins: ..., leaves: ...}`
  in the same shape, ready to carry as a `Phoenix.Socket.Broadcast`'s
  `"presence_diff"` payload.

  Pure and process-free on purpose: the reader/tracker wiring that decides
  *when* to call this lives in `TrogonPresence.Tracker`, so the shaping
  itself can be exercised without a NATS connection.
  """

  alias TrogonPresence.{Diff, Key, MetaEntry, Presences}

  @type phoenix_presences :: %{optional(String.t()) => %{metas: [map()]}}

  @spec presences(Presences.t()) :: phoenix_presences()
  def presences(presences) when is_map(presences) do
    Map.new(presences, fn {key, metas} ->
      {Key.raw(key), %{metas: Enum.map(metas, &meta/1)}}
    end)
  end

  @spec diff_payload(Diff.t()) :: %{joins: phoenix_presences(), leaves: phoenix_presences()}
  def diff_payload(%Diff{joins: joins, leaves: leaves}) do
    %{joins: presences(joins), leaves: presences(leaves)}
  end

  @spec broadcast(String.t(), map()) :: Phoenix.Socket.Broadcast.t()
  def broadcast(topic, payload) do
    %Phoenix.Socket.Broadcast{topic: topic, event: "presence_diff", payload: payload}
  end

  defp meta(%MetaEntry{} = entry) do
    Map.new(MetaEntry.encode(entry), fn
      {"phx_ref", value} -> {:phx_ref, value}
      {"phx_ref_prev", value} -> {:phx_ref_prev, value}
      {key, value} -> {key, value}
    end)
  end
end
