defmodule TrogonPresence.ShardCount do
  @moduledoc """
  The number of shards a deployment is partitioned into. Matches
  `trogon_presence::shard::ShardCount`: a power of two between 64 and 1024,
  with FNV-1a 64-bit hashing and bitwise-AND masking used to place a key or
  topic onto a shard.
  """

  import Bitwise

  alias TrogonPresence.{Shard, ViewShard, WriterShard}

  @enforce_keys [:count]
  defstruct [:count]
  @type t :: %__MODULE__{count: pos_integer()}

  @min 64
  @max 1024
  @token_prefix "s"
  @fnv1a64_offset_basis 0xCBF29CE484222325
  @fnv1a64_prime 0x100000001B3
  @u64_mask (1 <<< 64) - 1

  @type error ::
          {:invalid_count, integer()}
          | {:out_of_range, non_neg_integer(), pos_integer()}
          | {:malformed_token, binary()}

  @spec default() :: t()
  def default, do: %__MODULE__{count: @min}

  @spec new(integer()) :: {:ok, t()} | {:error, error()}
  def new(count) when is_integer(count) do
    if power_of_two?(count) and count in @min..@max do
      {:ok, %__MODULE__{count: count}}
    else
      {:error, {:invalid_count, count}}
    end
  end

  @spec new!(integer()) :: t()
  def new!(count) do
    case new(count) do
      {:ok, shard_count} -> shard_count
      {:error, reason} -> raise ArgumentError, "invalid shard count #{count}: #{inspect(reason)}"
    end
  end

  @spec get(t()) :: pos_integer()
  def get(%__MODULE__{count: count}), do: count

  @spec token_width(t()) :: pos_integer()
  def token_width(%__MODULE__{count: count}) do
    count |> Kernel.-(1) |> Integer.to_string() |> byte_size()
  end

  @spec shard(t(), non_neg_integer()) :: {:ok, Shard.t()} | {:error, error()}
  def shard(%__MODULE__{count: count}, index) when is_integer(index) and index >= 0 do
    if index < count do
      {:ok, Shard.new(index)}
    else
      {:error, {:out_of_range, index, count}}
    end
  end

  @spec shards(t()) :: [Shard.t()]
  def shards(%__MODULE__{count: count}), do: Enum.map(0..(count - 1), &Shard.new/1)

  @spec fnv1a64(binary()) :: non_neg_integer()
  def fnv1a64(bytes) when is_binary(bytes) do
    bytes
    |> :binary.bin_to_list()
    |> Enum.reduce(@fnv1a64_offset_basis, fn byte, hash ->
      (bxor(hash, byte) * @fnv1a64_prime) &&& @u64_mask
    end)
  end

  @doc """
  Hashes `bytes` and masks the result down to a shard index for this count.
  Exposed for `ViewShard.of/2` and `WriterShard.of/2`; `trogon_presence`
  keeps the equivalent `masked` method module-private since both callers
  live in the same Rust source file.
  """
  @spec masked(t(), binary()) :: Shard.t()
  def masked(%__MODULE__{count: count}, bytes) when is_binary(bytes) do
    index = fnv1a64(bytes) &&& count - 1
    Shard.new(index)
  end

  @spec token(t(), Shard.t() | ViewShard.t() | WriterShard.t()) :: binary()
  def token(%__MODULE__{} = count, shard_like) do
    shard = to_shard(shard_like)
    width = token_width(count)
    @token_prefix <> String.pad_leading(Integer.to_string(Shard.index(shard)), width, "0")
  end

  @spec parse_token(t(), binary()) :: {:ok, Shard.t()} | {:error, error()}
  def parse_token(%__MODULE__{} = count, token) when is_binary(token) do
    width = token_width(count)

    with @token_prefix <> digits <- token,
         true <- byte_size(digits) == width,
         true <- digits != "" and String.match?(digits, ~r/^[0-9]+$/),
         {index, ""} <- Integer.parse(digits) do
      shard(count, index)
    else
      _ -> {:error, {:malformed_token, token}}
    end
  end

  defp to_shard(%Shard{} = shard), do: shard
  defp to_shard(%ViewShard{} = view_shard), do: ViewShard.shard(view_shard)
  defp to_shard(%WriterShard{} = writer_shard), do: WriterShard.shard(writer_shard)

  defp power_of_two?(n) when is_integer(n) and n > 0, do: (n &&& n - 1) == 0
  defp power_of_two?(_n), do: false
end
