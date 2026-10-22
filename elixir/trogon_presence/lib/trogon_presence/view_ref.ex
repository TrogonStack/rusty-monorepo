defmodule TrogonPresence.ViewRef do
  @moduledoc """
  The opaque `phx_ref` a reader's view of one meta entry carries on the wire.
  Matches `trogon_presence::phx_ref::ViewRef`: any non-empty string up to 64
  bytes. The server derives this from a `StoredRef` and its meta via SHA-256,
  but the shim never derives one itself; it only parses what the service
  publishes, so it accepts the same wire shape without recomputing the
  digest.
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
      {:error, reason} -> raise ArgumentError, "invalid view ref #{inspect(value)}: #{reason}"
    end
  end

  @spec to_string(t()) :: binary()
  def to_string(%__MODULE__{value: value}), do: value

  defimpl String.Chars do
    def to_string(%TrogonPresence.ViewRef{value: value}), do: value
  end

  defimpl Jason.Encoder do
    def encode(%TrogonPresence.ViewRef{value: value}, opts), do: Jason.Encode.string(value, opts)
  end
end
