defmodule TrogonPresence.Meta do
  @moduledoc """
  Presence metadata attached to a tracked entry. Matches
  `trogon_presence::meta::Meta`: a JSON object capped at 4096 encoded bytes
  and 32 levels of nesting, with `__proto__`/`constructor`/`prototype`
  forbidden at any depth and `phx_ref`/`phx_ref_prev` reserved at the top
  level (the server injects those itself).

  Elixir maps cannot hold duplicate keys, so unlike the Rust
  `CanonicalJsonV1` decoder this module has no duplicate-member check to
  port: the invariant that decoder enforces by rejecting repeated JSON
  object keys is already guaranteed by the host language's data structure.
  """

  @enforce_keys [:map]
  defstruct [:map]
  @type t :: %__MODULE__{map: map()}

  @reserved_keys ~w(__proto__ constructor prototype phx_ref phx_ref_prev)
  @prototype_keys ~w(__proto__ constructor prototype)
  @max_depth 32
  @max_encoded_bytes 4096

  @type error :: {:reserved_key, binary()} | :too_deep | :too_large | :not_a_map

  @spec new(map()) :: {:ok, t()} | {:error, error()}
  def new(map) when is_map(map) do
    with :ok <- check_reserved_top_level(map),
         :ok <- check_prototype_keys(map),
         :ok <- check_depth(map),
         {:ok, encoded} <- encode(map),
         :ok <- check_size(encoded) do
      {:ok, %__MODULE__{map: map}}
    end
  end

  def new(_not_a_map), do: {:error, :not_a_map}

  @spec new!(map()) :: t()
  def new!(map) do
    case new(map) do
      {:ok, meta} -> meta
      {:error, reason} -> raise ArgumentError, "invalid meta #{inspect(map)}: #{inspect(reason)}"
    end
  end

  @spec to_map(t()) :: map()
  def to_map(%__MODULE__{map: map}), do: map

  defp check_reserved_top_level(map) do
    Enum.find_value(@reserved_keys, :ok, fn reserved ->
      if has_key?(map, reserved), do: {:error, {:reserved_key, reserved}}
    end)
  end

  defp has_key?(map, key) when is_binary(key) do
    Map.has_key?(map, key) or Map.has_key?(map, String.to_atom(key))
  end

  defp check_prototype_keys(map) do
    case find_prototype_key(map) do
      nil -> :ok
      key -> {:error, {:reserved_key, key}}
    end
  end

  defp find_prototype_key(value) when is_map(value) do
    Enum.find_value(value, fn {key, member} ->
      if prototype_key?(key), do: key_to_binary(key), else: find_prototype_key(member)
    end)
  end

  defp find_prototype_key(value) when is_list(value) do
    Enum.find_value(value, &find_prototype_key/1)
  end

  defp find_prototype_key(_scalar), do: nil

  defp prototype_key?(key) when is_binary(key), do: key in @prototype_keys
  defp prototype_key?(key) when is_atom(key), do: Atom.to_string(key) in @prototype_keys
  defp prototype_key?(_key), do: false

  defp key_to_binary(key) when is_binary(key), do: key
  defp key_to_binary(key) when is_atom(key), do: Atom.to_string(key)

  defp check_depth(map) do
    if depth_of(map) > @max_depth, do: {:error, :too_deep}, else: :ok
  end

  defp depth_of(value) when is_map(value) do
    1 + (value |> Map.values() |> Enum.map(&depth_of/1) |> Enum.max(fn -> 0 end))
  end

  defp depth_of(value) when is_list(value) do
    1 + (value |> Enum.map(&depth_of/1) |> Enum.max(fn -> 0 end))
  end

  defp depth_of(_scalar), do: 0

  defp encode(map) do
    case Jason.encode(map) do
      {:ok, encoded} -> {:ok, encoded}
      {:error, _reason} -> {:error, :not_a_map}
    end
  end

  defp check_size(encoded) do
    if byte_size(encoded) > @max_encoded_bytes, do: {:error, :too_large}, else: :ok
  end

  defimpl Jason.Encoder do
    def encode(%TrogonPresence.Meta{map: map}, opts), do: Jason.Encode.map(map, opts)
  end
end
