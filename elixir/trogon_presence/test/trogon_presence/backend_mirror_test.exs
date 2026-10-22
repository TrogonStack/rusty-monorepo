defmodule TrogonPresence.BackendMirrorTest do
  use TrogonPresence.Test.PresenceCase, async: false

  alias TrogonPresence.Test.{DualPresence, NatsReadPresence}

  setup %{conn: conn} do
    start_supervised!({Phoenix.PubSub, name: TrogonPresence.Test.PubSub})
    start_supervised!({DualPresence, conn: conn})
    start_supervised!({NatsReadPresence, conn: conn})
    :ok
  end

  describe "a meta Phoenix accepts but this shim's own validation rejects" do
    test "stays tracked through :dual, since Phoenix is authoritative for the call's result" do
      identity = fresh_identity()
      topic = "mirror-dual-#{identity.key}"
      reserved_meta = %{"__proto__" => true}

      assert {:ok, _ref} = DualPresence.track(self(), topic, "alice", reserved_meta)

      assert %{"alice" => %{metas: [%{"__proto__" => true}]}} = DualPresence.list(topic)
    end

    test "never reaches NATS-sourced reads through :nats_read, and stays unmirrored forever" do
      identity = fresh_identity()
      topic = "mirror-nats-read-#{identity.key}"
      reserved_meta = %{"__proto__" => true}

      assert {:ok, _ref} = NatsReadPresence.track(self(), topic, "alice", reserved_meta)

      refute Map.has_key?(NatsReadPresence.list(topic), "alice")

      Process.sleep(200)
      refute Map.has_key?(NatsReadPresence.list(topic), "alice")
    end
  end

  describe "stage transition" do
    test "a key written through :dual converges into :nats_read's NATS-sourced list/1" do
      identity = fresh_identity()
      topic = "convergence-#{identity.key}"

      assert {:ok, dual_ref} = DualPresence.track(self(), topic, "alice", %{"status" => "online"})

      wait_until(fn -> Map.has_key?(NatsReadPresence.list(topic), "alice") end)

      dual_view = DualPresence.list(topic)
      nats_read_view = NatsReadPresence.list(topic)

      assert %{"alice" => %{metas: [%{"status" => "online"} = dual_meta]}} = dual_view
      assert %{"alice" => %{metas: [%{"status" => "online"} = nats_meta]}} = nats_read_view

      assert Map.drop(dual_meta, [:phx_ref, :phx_ref_prev]) ==
               Map.drop(nats_meta, [:phx_ref, :phx_ref_prev])

      # The ref itself is allowed to differ across the boundary: :dual hands
      # back Phoenix's own ref, :nats_read hands back the ref NATS minted for
      # the mirror write, and the two stores never agree on who authored it.
      refute dual_ref == nats_meta[:phx_ref]
    end
  end

  defp wait_until(fun, attempts \\ 100) do
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
