defmodule TrogonPresence.ShardTest do
  use ExUnit.Case, async: true

  alias TrogonPresence.{Key, ShardCount, Topic, ViewShard, WriterShard}

  test "fnv1a64 matches the rust reference vectors" do
    assert ShardCount.fnv1a64("") == 0xCBF29CE484222325
    assert ShardCount.fnv1a64("a") == 0xAF63DC4C8601EC8C
    assert ShardCount.fnv1a64("room:lobby") == 0x138BB1F561758538
    assert ShardCount.fnv1a64("room:josé") == 0x2FE7E9FC55B34E80
  end

  test "shards a topic onto the default count the same as the rust reference" do
    {:ok, topic} = Topic.new("room:lobby")
    default = ShardCount.default()
    assert ShardCount.token(default, ViewShard.of(topic, default)) == "s56"

    {:ok, large} = ShardCount.new(1024)
    assert ShardCount.token(large, ViewShard.of(topic, large)) == "s0312"
  end

  test "writer and view shards hash raw bytes and can differ for the same entry" do
    count = ShardCount.default()
    {:ok, key} = Key.new("ana")
    {:ok, topic} = Topic.new("room:lobby")

    writer = WriterShard.of(key, count)
    view = ViewShard.of(topic, count)

    assert ShardCount.token(count, view) == "s56"
    assert WriterShard.shard(writer) != ViewShard.shard(view)
  end

  test "rejects a count that is not a power of two in range" do
    for bad <- [0, 1, 32, 63, 65, 96, 2048] do
      assert {:error, {:invalid_count, ^bad}} = ShardCount.new(bad)
    end

    for good <- [64, 128, 256, 512, 1024] do
      assert {:ok, _} = ShardCount.new(good)
    end
  end

  test "parses shard tokens strictly" do
    count = ShardCount.default()
    assert {:ok, shard} = ShardCount.parse_token(count, "s07")
    assert ShardCount.token(count, shard) == "s07"

    for bad <- ["s7", "s007", "07", "t07", "s+7", "s64", "s0x"] do
      assert {:error, _} = ShardCount.parse_token(count, bad)
    end
  end
end
