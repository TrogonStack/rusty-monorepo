defmodule TrogonPresence.Snapshot.Digest do
  @moduledoc """
  The SHA-256 digest a snapshot manifest carries over its parts, as 64
  lowercase hex characters. Matches
  `trogon_presence_service::snapshot::SnapshotDigest`.
  """

  @enforce_keys [:hex]
  defstruct [:hex]
  @type t :: %__MODULE__{hex: binary()}

  @spec parse(binary()) :: {:ok, t()} | :error
  def parse(hex) when is_binary(hex) do
    if byte_size(hex) == 64 and String.match?(hex, ~r/^[0-9a-f]{64}$/) do
      {:ok, %__MODULE__{hex: hex}}
    else
      :error
    end
  end

  def parse(_hex), do: :error

  @spec of([binary()]) :: t()
  def of(parts) when is_list(parts) do
    hex = :sha256 |> :crypto.hash(IO.iodata_to_binary(parts)) |> Base.encode16(case: :lower)
    %__MODULE__{hex: hex}
  end
end
