defmodule TrogonPresence.SubjectsTest do
  use ExUnit.Case, async: true

  alias TrogonPresence.{ConnectionId, Key, ShardCount, SnapshotId, Subjects, Topic, ViewShard}

  test "builds write subjects matching the rust reference" do
    {:ok, topic} = Topic.new("room:lobby")
    {:ok, key} = Key.new("carol")

    assert Subjects.track(key, topic) == "presence.v1.track.carol.room.lobby"
    assert Subjects.update(key, topic) == "presence.v1.update.carol.room.lobby"
    assert Subjects.untrack(key, topic) == "presence.v1.untrack.carol.room.lobby"
    assert Subjects.diff(topic) == "presence.v1.diff.room.lobby"
    assert Subjects.diff_filter() == "presence.v1.diff.>"
  end

  test "builds holder subjects matching the rust reference" do
    {:ok, key} = Key.new("ana@x.io")
    assert Subjects.heartbeat(key) == "presence.v1.heartbeat.ana=40x=2Eio"
    assert Subjects.release(key) == "presence.v1.release.ana=40x=2Eio"
  end

  test "builds the epoch subject from a shard token" do
    shards = ShardCount.default()
    {:ok, topic} = Topic.new("room:lobby")
    shard = ViewShard.of(topic, shards)
    assert Subjects.epoch(shards, shard) == "presence.v1.epoch.s56"
    assert Subjects.epoch_filter() == "presence.v1.epoch.*"
  end

  test "builds the snapshot request and reply subjects matching the rust reference" do
    shards = ShardCount.default()
    {:ok, topic} = Topic.new("room:lobby")
    {:ok, key} = Key.new("ana@x.io")
    connection = ConnectionId.from_bytes(<<3::128>>)

    subject = Subjects.snapshot_request(shards, key, connection, topic)
    assert subject == "presence.v1.snapshot.s56.ana=40x=2Eio.#{connection}.room.lobby"

    assert Subjects.snapshot_reply_filter(key, connection) ==
             "presence.v1.snapshot-reply.ana=40x=2Eio.#{connection}.*"

    snapshot = SnapshotId.from_bytes(<<4::128>>)

    assert Subjects.snapshot_reply(key, connection, snapshot) ==
             "presence.v1.snapshot-reply.ana=40x=2Eio.#{connection}.#{snapshot}"
  end
end
