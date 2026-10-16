defmodule TrogonPresence.HeartbeatLeaseLossTest do
  @moduledoc """
  Runs its own `nats-server`/`trogon-presence` pair instead of reusing
  `TrogonPresence.Test.PresenceCase`'s shared, module-level one: this is the
  only test that needs a non-default `TROGON_PRESENCE_LEASE_TTL`, and the
  shared pair is already running under the default lease by the time any
  test module's `setup_all` would want to change it.
  """

  use ExUnit.Case, async: false

  alias TrogonPresence.Test.{NatsServer, Presence, PresenceService}

  setup_all do
    nats = NatsServer.start!()
    {:ok, conn} = Gnat.start_link(parse_url(nats.url))

    service =
      PresenceService.start!(nats.url, conn, [
        {"TROGON_PRESENCE_LEASE_TTL", "1s"},
        {"TROGON_PRESENCE_HEARTBEAT_INTERVAL", "300ms"}
      ])

    on_exit(fn ->
      Gnat.stop(conn)
      PresenceService.stop(service)
      NatsServer.stop(nats)
    end)

    %{conn: conn}
  end

  setup %{conn: conn} do
    start_supervised!({Phoenix.PubSub, name: TrogonPresence.Test.PubSub})
    start_supervised!({Presence, conn: conn, heartbeat_ms: 1_500})
    :ok
  end

  test "a holder whose lease lapses before the Tracker's next heartbeat tick is re-tracked instead of disappearing" do
    suffix = System.unique_integer([:positive, :monotonic])
    topic = "presence:lease-loss:#{suffix}"
    key = "holder-#{suffix}"

    assert {:ok, _ref} = Presence.track(self(), topic, key, %{"status" => "online"})

    Process.sleep(4_000)

    wait_until(fn -> Map.has_key?(Presence.list(topic), key) end)
    assert %{^key => %{metas: [%{"status" => "online"}]}} = Presence.list(topic)
  end

  defp wait_until(fun, attempts \\ 50) do
    cond do
      fun.() ->
        :ok

      attempts <= 1 ->
        flunk("condition not met in time")

      true ->
        Process.sleep(100)
        wait_until(fun, attempts - 1)
    end
  end

  defp parse_url("nats://" <> host_port) do
    [host, port] = String.split(host_port, ":")
    %{host: host, port: String.to_integer(port)}
  end
end
