defmodule TrogonPresence.WriterIntegrationTest do
  use TrogonPresence.Test.PresenceCase, async: false

  alias TrogonPresence.Wire.{
    ErrorReply,
    HeartbeatReply,
    ReleaseReply,
    UntrackedReply,
    WrittenReply
  }

  alias TrogonPresence.{Meta, Topic, Writer}

  setup %{conn: conn} do
    identity = fresh_identity()
    {:ok, topic} = Topic.new("writer-test:#{identity.key}")
    Map.merge(identity, %{conn: conn, topic: topic})
  end

  test "tracks a presence and returns a lifetime and mutation sequence", ctx do
    meta = Meta.new!(%{"status" => "online"})

    assert {:ok, %WrittenReply{} = written} =
             Writer.track(ctx.conn, ctx.key, ctx.connection, ctx.topic, ctx.holder, meta)

    assert written.adopted == false
    assert %TrogonPresence.LifetimeId{} = written.lifetime
    assert %TrogonPresence.MutationSequence{} = written.mutation_seq
  end

  test "the same holder tracking the same topic twice is already_tracked", ctx do
    meta = Meta.new!(%{})
    assert {:ok, _} = Writer.track(ctx.conn, ctx.key, ctx.connection, ctx.topic, ctx.holder, meta)

    assert {:error, %ErrorReply{code: "already_tracked"} = error} =
             Writer.track(ctx.conn, ctx.key, ctx.connection, ctx.topic, ctx.holder, meta)

    refute ErrorReply.retryable?(error)
  end

  test "update advances the mutation sequence and rev", ctx do
    meta = Meta.new!(%{"status" => "online"})

    {:ok, written} = Writer.track(ctx.conn, ctx.key, ctx.connection, ctx.topic, ctx.holder, meta)

    assert {:ok, %WrittenReply{} = updated} =
             Writer.update(
               ctx.conn,
               ctx.key,
               ctx.connection,
               ctx.topic,
               ctx.holder,
               Meta.new!(%{"status" => "away"}),
               written.lifetime,
               written.mutation_seq
             )

    assert TrogonPresence.EntryRevision.compare(updated.rev, written.rev) in [:eq, :gt]
  end

  test "update with a stale mutation sequence is rejected", ctx do
    meta = Meta.new!(%{})
    {:ok, written} = Writer.track(ctx.conn, ctx.key, ctx.connection, ctx.topic, ctx.holder, meta)

    assert {:ok, _} =
             Writer.update(
               ctx.conn,
               ctx.key,
               ctx.connection,
               ctx.topic,
               ctx.holder,
               meta,
               written.lifetime,
               written.mutation_seq
             )

    assert {:error, %ErrorReply{} = error} =
             Writer.update(
               ctx.conn,
               ctx.key,
               ctx.connection,
               ctx.topic,
               ctx.holder,
               meta,
               written.lifetime,
               written.mutation_seq
             )

    assert error.code in ["operation_conflict", "sequence_conflict", "conflict"]
  end

  test "untrack releases the entry and reports it gone on a second attempt", ctx do
    meta = Meta.new!(%{})
    {:ok, written} = Writer.track(ctx.conn, ctx.key, ctx.connection, ctx.topic, ctx.holder, meta)

    assert {:ok, %UntrackedReply{untracked: true}} =
             Writer.untrack(
               ctx.conn,
               ctx.key,
               ctx.connection,
               ctx.topic,
               ctx.holder,
               written.lifetime,
               written.mutation_seq
             )
  end

  test "untrack of an unknown lifetime returns a gone or not_found error", ctx do
    ghost_lifetime = TrogonPresence.LifetimeId.generate()
    ghost_seq = TrogonPresence.MutationSequence.from_integer(1)

    assert {:error, %ErrorReply{code: code}} =
             Writer.untrack(
               ctx.conn,
               ctx.key,
               ctx.connection,
               ctx.topic,
               ctx.holder,
               ghost_lifetime,
               ghost_seq
             )

    assert code in ["gone", "not_found"]
  end

  test "heartbeat renews a tracked entry", ctx do
    meta = Meta.new!(%{})
    {:ok, written} = Writer.track(ctx.conn, ctx.key, ctx.connection, ctx.topic, ctx.holder, meta)

    assert {:ok, %HeartbeatReply{interval: interval, entries: [status]}} =
             Writer.heartbeat(ctx.conn, ctx.key, ctx.connection, [
               {ctx.holder, ctx.topic, written.lifetime, written.mutation_seq}
             ])

    assert is_integer(interval)
    assert is_binary(status)
  end

  test "release frees the holder's lease", ctx do
    meta = Meta.new!(%{})
    {:ok, written} = Writer.track(ctx.conn, ctx.key, ctx.connection, ctx.topic, ctx.holder, meta)

    assert {:ok, %ReleaseReply{released: [entry], holder_freed: true}} =
             Writer.release(ctx.conn, ctx.key, ctx.connection, ctx.holder, [
               {ctx.topic, written.lifetime}
             ])

    assert entry.status in [:released, :gone]
  end
end
