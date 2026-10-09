defmodule TrogonPresence.ViewShard do
  @moduledoc """
  The shard that owns the diff log and snapshot for a topic. Matches
  `trogon_presence::shard::ViewShard`: the topic's raw bytes hashed and
  masked onto the deployment's shard count.
  """

  alias TrogonPresence.{Shard, ShardCount, Topic}

  @enforce_keys [:shard]
  defstruct [:shard]
  @type t :: %__MODULE__{shard: Shard.t()}

  @spec of(Topic.t(), ShardCount.t()) :: t()
  def of(%Topic{} = topic, %ShardCount{} = count) do
    %__MODULE__{shard: ShardCount.masked(count, Topic.raw(topic))}
  end

  @spec shard(t()) :: Shard.t()
  def shard(%__MODULE__{shard: shard}), do: shard

  @spec from_shard(Shard.t()) :: t()
  def from_shard(%Shard{} = shard), do: %__MODULE__{shard: shard}
end
