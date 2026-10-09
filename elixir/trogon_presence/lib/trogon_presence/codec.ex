defmodule TrogonPresence.Codec do
  @moduledoc """
  The byte-escape codec used for every NATS subject token in the presence
  protocol. Ports `trogon_presence::codec` byte for byte: any byte outside
  `[A-Za-z0-9_-]` is escaped as `=XX` (uppercase hex), and an empty string
  encodes to a single `=`.
  """

  @escape ?=

  @spec encode(binary()) :: binary()
  def encode(<<>>), do: "="

  def encode(bytes) when is_binary(bytes) do
    if passthrough?(bytes) do
      bytes
    else
      bytes
      |> :binary.bin_to_list()
      |> Enum.map(&encode_byte/1)
      |> IO.iodata_to_binary()
    end
  end

  defp passthrough?(bytes), do: Enum.all?(:binary.bin_to_list(bytes), &passthrough_byte?/1)

  defp passthrough_byte?(byte), do: byte in ?0..?9 or byte in ?A..?Z or byte in ?a..?z or byte in [?_, ?-]

  defp encode_byte(byte) when byte in ?0..?9 or byte in ?A..?Z or byte in ?a..?z or byte in [?_, ?-] do
    <<byte>>
  end

  defp encode_byte(byte) do
    hex = byte |> Integer.to_string(16) |> String.upcase() |> String.pad_leading(2, "0")
    <<@escape, hex::binary>>
  end

  @spec decode(binary()) :: {:ok, binary()} | :error
  def decode("="), do: {:ok, ""}

  def decode(token) when is_binary(token) do
    case decode_tokens(token, []) do
      {:ok, decoded} ->
        if encode(decoded) == token, do: {:ok, decoded}, else: :error

      :error ->
        :error
    end
  end

  defp decode_tokens(<<>>, acc), do: {:ok, acc |> Enum.reverse() |> IO.iodata_to_binary()}

  defp decode_tokens(<<@escape, hex1, hex2, rest::binary>>, acc) when hex1 in ?0..?9 or hex1 in ?A..?F do
    if hex2 in ?0..?9 or hex2 in ?A..?F do
      case Integer.parse(<<hex1, hex2>>, 16) do
        {byte, ""} -> decode_tokens(rest, [<<byte>> | acc])
        _ -> :error
      end
    else
      :error
    end
  end

  defp decode_tokens(<<@escape, _rest::binary>>, _acc), do: :error

  defp decode_tokens(<<byte, rest::binary>>, acc), do: decode_tokens(rest, [<<byte>> | acc])
end
