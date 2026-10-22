defmodule TrogonPresence.Test.PresenceCase do
  @moduledoc """
  Starts one real `nats-server` and one real `trogon-presence run` for a test
  module, connects a `Gnat` client to it, and guarantees both OS processes
  are stopped when the module finishes, including when a test fails.

  Shared once per module (`setup_all`) rather than per test: starting a
  server and a service is too slow to pay per test, and each test already
  gets its own key/topic/holder/connection, so sharing the pair does not
  leak state between tests.
  """

  use ExUnit.CaseTemplate

  alias TrogonPresence.Test.{NatsServer, PresenceService}

  using do
    quote do
      import TrogonPresence.Test.PresenceCase, only: [fresh_identity: 0]
    end
  end

  setup_all do
    nats = NatsServer.start!()
    {:ok, conn} = Gnat.start_link(parse_url(nats.url))
    service = PresenceService.start!(nats.url, conn)

    on_exit(fn ->
      Gnat.stop(conn)
      PresenceService.stop(service)
      NatsServer.stop(nats)
    end)

    %{conn: conn, nats: nats, service: service}
  end

  @spec fresh_identity() :: %{
          key: TrogonPresence.Key.t(),
          connection: TrogonPresence.ConnectionId.t(),
          holder: TrogonPresence.HolderId.t()
        }
  def fresh_identity do
    suffix = System.unique_integer([:positive, :monotonic])
    {:ok, key} = TrogonPresence.Key.new("writer-test-#{suffix}")

    %{
      key: key,
      connection: TrogonPresence.ConnectionId.generate(),
      holder: TrogonPresence.HolderId.generate()
    }
  end

  defp parse_url("nats://" <> host_port) do
    [host, port] = String.split(host_port, ":")
    %{host: host, port: String.to_integer(port)}
  end
end
