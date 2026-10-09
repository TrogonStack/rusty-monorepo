defmodule TrogonPresence.Key do
  @moduledoc """
  The presence caller identity (the `key` a client tracks, updates, lists,
  and gets under). Matches `trogon_presence::key::PresenceKey`: a raw string
  bounded at 256 bytes, with a `=`-escaped NATS subject token bounded at 768
  bytes once escaped.
  """

  alias TrogonPresence.Codec

  @enforce_keys [:raw, :token]
  defstruct [:raw, :token]
  @type t :: %__MODULE__{raw: binary(), token: binary()}

  @max_raw_bytes 256
  @max_escaped_bytes 768

  @type error :: :empty | :too_long | :escaped_too_long

  @spec new(binary()) :: {:ok, t()} | {:error, error()}
  def new(raw) when is_binary(raw) do
    cond do
      byte_size(raw) == 0 -> {:error, :empty}
      byte_size(raw) > @max_raw_bytes -> {:error, :too_long}
      true ->
        token = Codec.encode(raw)

        if byte_size(token) > @max_escaped_bytes do
          {:error, :escaped_too_long}
        else
          {:ok, %__MODULE__{raw: raw, token: token}}
        end
    end
  end

  @spec new!(binary()) :: t()
  def new!(raw) do
    case new(raw) do
      {:ok, key} -> key
      {:error, reason} -> raise ArgumentError, "invalid presence key #{inspect(raw)}: #{reason}"
    end
  end

  @spec from_token(binary()) :: {:ok, t()} | {:error, error() | :malformed}
  def from_token(token) when is_binary(token) do
    case Codec.decode(token) do
      {:ok, raw} ->
        case new(raw) do
          {:ok, key} when key.token == token -> {:ok, key}
          {:ok, _key} -> {:error, :malformed}
          error -> error
        end

      :error ->
        {:error, :malformed}
    end
  end

  @spec raw(t()) :: binary()
  def raw(%__MODULE__{raw: raw}), do: raw

  @spec token(t()) :: binary()
  def token(%__MODULE__{token: token}), do: token

  defimpl String.Chars do
    def to_string(%TrogonPresence.Key{raw: raw}), do: raw
  end

  defimpl Jason.Encoder do
    def encode(%TrogonPresence.Key{raw: raw}, opts), do: Jason.Encode.string(raw, opts)
  end
end
