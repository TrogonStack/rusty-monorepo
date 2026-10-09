defmodule TrogonPresence.Topic do
  @moduledoc """
  A presence topic. Matches `trogon_presence::topic::Topic`: `:`-separated
  segments, each `=`-escaped into its own NATS subject token and joined with
  `.`. Segments are bounded at 128 raw / 384 escaped bytes, the whole topic
  at 256 raw / 768 escaped bytes.
  """

  alias TrogonPresence.Codec

  @enforce_keys [:raw, :tokens]
  defstruct [:raw, :tokens]
  @type t :: %__MODULE__{raw: binary(), tokens: binary()}

  @segment_max_raw_bytes 128
  @segment_max_escaped_bytes 384
  @max_raw_bytes 256
  @max_escaped_bytes 768

  @type error :: :empty | :too_long | :segment_too_long | :escaped_too_long | :segment_escaped_too_long

  @spec new(binary()) :: {:ok, t()} | {:error, error()}
  def new(raw) when is_binary(raw) do
    cond do
      byte_size(raw) == 0 -> {:error, :empty}
      byte_size(raw) > @max_raw_bytes -> {:error, :too_long}
      true -> encode_segments(String.split(raw, ":"), raw)
    end
  end

  @spec new!(binary()) :: t()
  def new!(raw) do
    case new(raw) do
      {:ok, topic} -> topic
      {:error, reason} -> raise ArgumentError, "invalid topic #{inspect(raw)}: #{reason}"
    end
  end

  @spec from_tokens(binary()) :: {:ok, t()} | {:error, error() | :malformed}
  def from_tokens(tokens) when is_binary(tokens) do
    with {:ok, segments} <- decode_tokens(String.split(tokens, ".")) do
      raw = Enum.join(segments, ":")

      case new(raw) do
        {:ok, topic} when topic.tokens == tokens -> {:ok, topic}
        {:ok, _topic} -> {:error, :malformed}
        error -> error
      end
    end
  end

  @spec raw(t()) :: binary()
  def raw(%__MODULE__{raw: raw}), do: raw

  @spec tokens(t()) :: binary()
  def tokens(%__MODULE__{tokens: tokens}), do: tokens

  @spec segments(t()) :: [binary()]
  def segments(%__MODULE__{raw: raw}), do: String.split(raw, ":")

  defp encode_segments(segments, raw) do
    Enum.reduce_while(segments, [], fn segment, acc ->
      cond do
        byte_size(segment) > @segment_max_raw_bytes ->
          {:halt, {:error, :segment_too_long}}

        true ->
          token = Codec.encode(segment)

          if byte_size(token) > @segment_max_escaped_bytes do
            {:halt, {:error, :segment_escaped_too_long}}
          else
            {:cont, [token | acc]}
          end
      end
    end)
    |> case do
      {:error, _reason} = error ->
        error

      reversed ->
        tokens = reversed |> Enum.reverse() |> Enum.join(".")

        if byte_size(tokens) > @max_escaped_bytes do
          {:error, :escaped_too_long}
        else
          {:ok, %__MODULE__{raw: raw, tokens: tokens}}
        end
    end
  end

  defp decode_tokens(tokens) do
    Enum.reduce_while(tokens, [], fn token, acc ->
      case Codec.decode(token) do
        {:ok, segment} -> {:cont, [segment | acc]}
        :error -> {:halt, {:error, :malformed}}
      end
    end)
    |> case do
      {:error, _reason} = error -> error
      reversed -> {:ok, Enum.reverse(reversed)}
    end
  end

  defimpl String.Chars do
    def to_string(%TrogonPresence.Topic{raw: raw}), do: raw
  end

  defimpl Jason.Encoder do
    def encode(%TrogonPresence.Topic{raw: raw}, opts), do: Jason.Encode.string(raw, opts)
  end
end
