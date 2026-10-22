defmodule TrogonPresence.ReaderIntegrationTest do
  use TrogonPresence.Test.PresenceCase, async: false

  alias TrogonPresence.{Meta, Reader, Topic, Writer}

  setup %{conn: conn} do
    identity = fresh_identity()
    {:ok, topic} = Topic.new("reader-test:#{identity.key}")
    Map.merge(identity, %{conn: conn, topic: topic})
  end

  test "installs an empty snapshot, then follows a join and a leave diff", ctx do
    {:ok, reader} =
      Reader.start_link(
        conn: ctx.conn,
        key: ctx.key,
        connection: ctx.connection,
        topic: ctx.topic,
        subscribers: [self()]
      )

    assert_receive {:trogon_presence_reader, ^reader, {:snapshot, _cursor, %{}}}, 5_000
    assert Reader.presences(reader) == %{}

    meta = Meta.new!(%{"status" => "online"})

    {:ok, written} =
      Writer.track(ctx.conn, ctx.key, ctx.connection, ctx.topic, ctx.holder, meta)

    assert_receive {:trogon_presence_reader, ^reader, {:diff, _cursor, joined}}, 5_000
    assert [joined_entry] = Map.fetch!(joined.joins, ctx.key)
    assert joined_entry.meta == meta
    assert joined.leaves == %{}

    presences = Reader.presences(reader)
    assert [present_entry] = Map.fetch!(presences, ctx.key)
    assert present_entry.phx_ref == joined_entry.phx_ref

    assert {:ok, _untracked} =
             Writer.untrack(
               ctx.conn,
               ctx.key,
               ctx.connection,
               ctx.topic,
               ctx.holder,
               written.lifetime,
               written.mutation_seq
             )

    assert_receive {:trogon_presence_reader, ^reader, {:diff, _cursor, left}}, 5_000
    assert [left_entry] = Map.fetch!(left.leaves, ctx.key)
    assert left_entry.phx_ref == joined_entry.phx_ref
    assert Reader.presences(reader) == %{}
  end
end
