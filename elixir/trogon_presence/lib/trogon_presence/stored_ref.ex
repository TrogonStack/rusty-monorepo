defmodule TrogonPresence.StoredRef do
  @moduledoc """
  The server-generated opaque ref for one stored entry. Matches
  `trogon_presence::phx_ref::StoredRef`: any non-empty string up to 64 bytes,
  not necessarily the canonical base64url shape the server mints for its own
  refs, since a foreign or hook-supplied ref may take any such form. The shim
  never generates one; it only stores what a track/update reply hands back.
  """

  @enforce_keys [:value]
  defstruct [:value]
  @type t :: %__MODULE__{value: binary()}

  @max_bytes 64

  @type error :: :empty | :too_long

  @spec new(binary()) :: {:ok, t()} | {:error, error()}
  def new(value) when is_binary(value) do
    cond do
      byte_size(value) == 0 -> {:error, :empty}
      byte_size(value) > @max_bytes -> {:error, :too_long}
      true -> {:ok, %__MODULE__{value: value}}
    end
  end

  @spec new!(binary()) :: t()
  def new!(value) do
    case new(value) do
      {:ok, ref} -> ref
      {:error, reason} -> raise ArgumentError, "invalid stored ref #{inspect(value)}: #{reason}"
    end
  end

  @spec to_string(t()) :: binary()
  def to_string(%__MODULE__{value: value}), do: value

  defimpl String.Chars do
    def to_string(%TrogonPresence.StoredRef{value: value}), do: value
  end

  defimpl Jason.Encoder do
    def encode(%TrogonPresence.StoredRef{value: value}, opts), do: Jason.Encode.string(value, opts)
  end
end
