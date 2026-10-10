defmodule TrogonPresence.ReconnectResnapshotTest do
  use TrogonPresence.Test.PresenceCase, async: false
  import Phoenix.ChannelTest

  @moduletag :capture_log

  @endpoint TrogonPresence.Test.Endpoint
  @resnapshot_ms :timer.minutes(5)
  @convergence_ms 1_000
  @quiet_ms 1_000

  alias TrogonPresence.{ConnectionId, HolderId, Key, Meta, Topic, Writer}
  alias TrogonPresence.Test.{Presence, TcpProxy, UserSocket}

  setup %{nats: nats} do
    proxy = start_supervised!({TcpProxy, nats_port(nats)})
    name = :"reconnect_gnat_#{System.unique_integer([:positive])}"

    start_supervised!(%{
      id: Gnat.ConnectionSupervisor,
      start:
        {Gnat.ConnectionSupervisor, :start_link,
         [
           %{
             name: name,
             backoff_period: 50,
             connection_settings: [%{host: "127.0.0.1", port: TcpProxy.port(proxy)}]
           }
         ]}
    })

    await_connection!(name)

    start_supervised!({Phoenix.PubSub, name: TrogonPresence.Test.PubSub})

    start_supervised!(
      {TrogonPresence.Test.Endpoint,
       pubsub_server: TrogonPresence.Test.PubSub,
       secret_key_base: String.duplicate("a", 64),
       server: false}
    )

    start_supervised!(
      {Presence, conn: name, resnapshot_ms: @resnapshot_ms, heartbeat_ms: @resnapshot_ms}
    )

    %{proxy: proxy, shim_conn: name}
  end

  test "a reconnect resnapshots at once and pushes each net change made while the shim was cut off",
       ctx do
    suffix = System.unique_integer([:positive, :monotonic])
    topic = "presence:reconnect-#{suffix}"
    {:ok, wire_topic} = Topic.new(topic)
    writer = ConnectionId.generate()

    {:ok, _reply, _socket} =
      UserSocket
      |> socket("socket_id", %{})
      |> subscribe_and_join(topic, %{"user_id" => "client"})

    assert_push("presence_state", _initial_state, 5_000)
    await_keys!(topic, ["client"])

    bob = track!(ctx.conn, writer, wire_topic, "bob")
    assert_push("presence_diff", %{joins: %{"bob" => _}, leaves: %{}}, 5_000)
    await_keys!(topic, ["bob", "client"])
    drain_pushes()

    cut_pid = GenServer.whereis(ctx.shim_conn)
    :ok = TcpProxy.cut(ctx.proxy)
    await_connection_down!(cut_pid)

    untrack!(ctx.conn, writer, wire_topic, bob)
    _carol = track!(ctx.conn, writer, wire_topic, "carol")
    dave = track!(ctx.conn, writer, wire_topic, "dave")
    untrack!(ctx.conn, writer, wire_topic, dave)

    refute_push("presence_diff", _payload, 300)
    assert keys(topic) == ["bob", "client"]

    :ok = TcpProxy.restore(ctx.proxy)

    assert_push("presence_diff", first, @convergence_ms)
    diffs = [first | collect_diffs(@quiet_ms)]

    assert Enum.flat_map(diffs, &Map.keys(&1.joins)) == ["carol"]
    assert Enum.flat_map(diffs, &Map.keys(&1.leaves)) == ["bob"]
    assert keys(topic) == ["carol", "client"]
  end

  defp track!(conn, connection, topic, raw_key) do
    key = Key.new!(raw_key)
    holder = HolderId.generate()

    {:ok, written} =
      Writer.track(conn, key, connection, topic, holder, Meta.new!(%{"status" => "online"}))

    %{key: key, holder: holder, written: written}
  end

  defp untrack!(conn, connection, topic, tracked) do
    {:ok, _untracked} =
      Writer.untrack(
        conn,
        tracked.key,
        connection,
        topic,
        tracked.holder,
        tracked.written.lifetime,
        tracked.written.mutation_seq
      )
  end

  defp keys(topic), do: topic |> Presence.list() |> Map.keys() |> Enum.sort()

  defp await_keys!(topic, expected), do: await!(fn -> keys(topic) == expected end)

  defp await_connection!(name), do: await!(fn -> is_pid(GenServer.whereis(name)) end)

  defp await_connection_down!(pid), do: await!(fn -> not Process.alive?(pid) end)

  defp await!(check, attempts \\ 300) do
    cond do
      check.() -> :ok
      attempts == 0 -> flunk("condition not met in time")
      true -> await_again(check, attempts)
    end
  end

  defp await_again(check, attempts) do
    receive do
    after
      10 -> await!(check, attempts - 1)
    end
  end

  defp collect_diffs(window_ms) do
    receive do
      %Phoenix.Socket.Message{event: "presence_diff", payload: payload} ->
        [payload | collect_diffs(window_ms)]
    after
      window_ms -> []
    end
  end

  defp drain_pushes do
    receive do
      %Phoenix.Socket.Message{} -> drain_pushes()
    after
      0 -> :ok
    end
  end

  defp nats_port(%{url: "nats://" <> host_port}) do
    [_host, port] = String.split(host_port, ":")
    String.to_integer(port)
  end
end
