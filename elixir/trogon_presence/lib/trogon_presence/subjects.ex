defmodule TrogonPresence.Subjects do
  @moduledoc """
  Builds the `presence.v1` NATS subjects a presence caller addresses.
  Mirrors `trogon_presence_service::subjects`, limited to the write, holder,
  diff, epoch and snapshot operations a client uses; the admin-only `list`
  and `get` read subjects are deliberately not built here.
  """

  alias TrogonPresence.{ConnectionId, Key, ShardCount, SnapshotId, Topic, ViewShard}

  @domain "presence.v1"

  @spec track(Key.t(), Topic.t()) :: String.t()
  def track(key, topic), do: write("track", key, topic)

  @spec update(Key.t(), Topic.t()) :: String.t()
  def update(key, topic), do: write("update", key, topic)

  @spec untrack(Key.t(), Topic.t()) :: String.t()
  def untrack(key, topic), do: write("untrack", key, topic)

  @spec heartbeat(Key.t()) :: String.t()
  def heartbeat(key), do: holder("heartbeat", key)

  @spec release(Key.t()) :: String.t()
  def release(key), do: holder("release", key)

  @spec diff(Topic.t()) :: String.t()
  def diff(topic), do: "#{@domain}.diff.#{Topic.tokens(topic)}"

  @spec diff_filter() :: String.t()
  def diff_filter, do: "#{@domain}.diff.>"

  @spec epoch(ShardCount.t(), ViewShard.t()) :: String.t()
  def epoch(shards, shard), do: "#{@domain}.epoch.#{ShardCount.token(shards, shard)}"

  @spec epoch_filter() :: String.t()
  def epoch_filter, do: "#{@domain}.epoch.*"

  @spec snapshot_request(ShardCount.t(), Key.t(), ConnectionId.t(), Topic.t()) :: String.t()
  def snapshot_request(shards, key, connection, topic) do
    shard = ShardCount.token(shards, ViewShard.of(topic, shards))
    "#{@domain}.snapshot.#{shard}.#{Key.token(key)}.#{connection}.#{Topic.tokens(topic)}"
  end

  @spec snapshot_reply_filter(Key.t(), ConnectionId.t()) :: String.t()
  def snapshot_reply_filter(key, connection) do
    "#{@domain}.snapshot-reply.#{Key.token(key)}.#{connection}.*"
  end

  @spec snapshot_reply(Key.t(), ConnectionId.t(), SnapshotId.t()) :: String.t()
  def snapshot_reply(key, connection, snapshot) do
    "#{@domain}.snapshot-reply.#{Key.token(key)}.#{connection}.#{snapshot}"
  end

  defp write(op, key, topic), do: "#{@domain}.#{op}.#{Key.token(key)}.#{Topic.tokens(topic)}"
  defp holder(op, key), do: "#{@domain}.#{op}.#{Key.token(key)}"
end
