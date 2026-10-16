defmodule TrogonPresence.Snapshot.Frame do
  @moduledoc """
  One wire frame of an in-flight snapshot: a part carrying a byte-range of
  the encoded `Presences`, or an end frame declaring the final part count.
  Matches `trogon_presence_service::snapshot::SnapshotFrame`.
  """

  alias TrogonPresence.Snapshot.Identity

  @header_kind "presence-kind"
  @header_part "presence-part"
  @header_parts "presence-parts"
  @kind_part "snapshot-part"
  @kind_end "snapshot-end"

  @type t ::
          {:part, Identity.t(), pos_integer(), binary()}
          | {:end, Identity.t(), pos_integer()}

  @spec identity(t()) :: Identity.t()
  def identity({:part, identity, _index, _bytes}), do: identity
  def identity({:end, identity, _parts}), do: identity

  @spec decode(%{headers: [{binary(), binary()}] | nil, body: binary()}) :: {:ok, t()} | :error
  def decode(%{headers: nil}), do: :error

  def decode(%{headers: headers, body: body}) do
    with {:ok, identity} <- Identity.from_headers(headers),
         {:ok, kind} <- header(headers, @header_kind) do
      decode_kind(kind, identity, headers, body)
    else
      _ -> :error
    end
  end

  def decode(_message), do: :error

  defp decode_kind(@kind_part, identity, headers, body) do
    with {:ok, raw} <- header(headers, @header_part),
         {index, ""} <- Integer.parse(raw),
         true <- index > 0 do
      {:ok, {:part, identity, index, body}}
    else
      _ -> :error
    end
  end

  defp decode_kind(@kind_end, identity, headers, _body) do
    with {:ok, raw} <- header(headers, @header_parts),
         {parts, ""} <- Integer.parse(raw),
         true <- parts > 0 do
      {:ok, {:end, identity, parts}}
    else
      _ -> :error
    end
  end

  defp decode_kind(_kind, _identity, _headers, _body), do: :error

  defp header(headers, name) do
    case List.keyfind(headers, name, 0) do
      {^name, value} -> {:ok, to_string(value)}
      nil -> :error
    end
  end
end
