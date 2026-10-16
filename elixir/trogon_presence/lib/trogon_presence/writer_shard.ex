defmodule TrogonPresence.WriterShard do
  @moduledoc """
  The shard that owns the write lease for a key. Matches
  `trogon_presence::shard::WriterShard`: the key's raw bytes hashed and
  masked onto the deployment's shard count.
  """

  alias TrogonPresence.{Key, Shard, ShardCount}

  @enforce_keys [:shard]
  defstruct [:shard]
  @type t :: %__MODULE__{shard: Shard.t()}

  @spec of(Key.t(), ShardCount.t()) :: t()
  def of(%Key{} = key, %ShardCount{} = count) do
    %__MODULE__{shard: ShardCount.masked(count, Key.raw(key))}
  end

  @spec shard(t()) :: Shard.t()
  def shard(%__MODULE__{shard: shard}), do: shard

  @spec from_shard(Shard.t()) :: t()
  def from_shard(%Shard{} = shard), do: %__MODULE__{shard: shard}
end
