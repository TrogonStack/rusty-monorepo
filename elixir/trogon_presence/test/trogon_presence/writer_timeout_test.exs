defmodule TrogonPresence.WriterTimeoutTest do
  use ExUnit.Case, async: false

  alias TrogonPresence.Test.NatsServer
  alias TrogonPresence.{HolderId, Key, Meta, Topic, Writer}

  test "a request nobody answers surfaces as :timeout rather than hanging forever" do
    nats = NatsServer.start!()
    on_exit(fn -> NatsServer.stop(nats) end)

    [host, port] = nats.url |> String.trim_leading("nats://") |> String.split(":")
    {:ok, conn} = Gnat.start_link(%{host: host, port: String.to_integer(port)})
    on_exit(fn -> if Process.alive?(conn), do: Gnat.stop(conn) end)

    {:ok, key} = Key.new("nobody-home")
    {:ok, topic} = Topic.new("nobody:home")
    connection = TrogonPresence.ConnectionId.generate()
    holder = HolderId.generate()

    assert {:error, :timeout} =
             Writer.track(conn, key, connection, topic, holder, Meta.new!(%{}), timeout: 100)
  end
end
