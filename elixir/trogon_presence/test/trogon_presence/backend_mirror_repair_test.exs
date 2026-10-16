defmodule TrogonPresence.BackendMirrorRepairTest do
  use TrogonPresence.Test.PresenceCase, async: false

  alias TrogonPresence.Test.{NatsReadPresence, PresenceService}

  @heartbeat_ms 100

  setup %{conn: conn} do
    start_supervised!({Phoenix.PubSub, name: TrogonPresence.Test.PubSub})
    start_supervised!({NatsReadPresence, conn: conn, heartbeat_ms: @heartbeat_ms})
    :ok
  end

  test "a transient mirror failure is repaired on the next heartbeat tick, not left unmirrored forever",
       %{conn: conn, nats: nats, service: service} do
    identity = fresh_identity()
    topic = "repair-#{identity.key}"

    PresenceService.stop(service)

    # The mirror write's own retry runs against the same deadline as a plain
    # `GenServer.call/3`, so a stopped service needs more than the default
    # 5 second call timeout to actually give up and fall through to Phoenix.
    assert {:ok, _ref} =
             GenServer.call(
               NatsReadPresence,
               {:track, self(), topic, "alice", %{"status" => "online"}},
               10_000
             )

    # The write went through on Phoenix alone while the service was down; the
    # NATS-sourced read this stage actually serves has nothing yet, but
    # proving that here would mean a `list/1` against a topic with no reader
    # yet, which starts one against the still-stopped service and blocks on
    # its own deadline instead of this call's. The repair below is the part
    # that matters: it covers the same gap once the service is back.
    replacement = PresenceService.start!(nats.url, conn)
    on_exit(fn -> PresenceService.stop(replacement) end)

    wait_until(fn -> Map.has_key?(NatsReadPresence.list(topic), "alice") end)

    assert %{"alice" => %{metas: [%{"status" => "online"}]}} = NatsReadPresence.list(topic)
  end

  defp wait_until(fun, attempts \\ 200) do
    cond do
      fun.() ->
        :ok

      attempts <= 1 ->
        flunk("condition not met in time")

      true ->
        Process.sleep(20)
        wait_until(fun, attempts - 1)
    end
  end
end
