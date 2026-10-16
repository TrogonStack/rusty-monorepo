defmodule TrogonPresence.BackendChannelTest do
  use TrogonPresence.Test.PresenceCase, async: false
  use Phoenix.ChannelTest

  @endpoint TrogonPresence.Test.Endpoint

  alias TrogonPresence.Test.{
    DualPresence,
    NatsReadPresence,
    PhoenixPresence,
    Presence,
    UserSocket
  }

  setup %{conn: conn} do
    start_supervised!({Phoenix.PubSub, name: TrogonPresence.Test.PubSub})

    start_supervised!(
      {TrogonPresence.Test.Endpoint,
       pubsub_server: TrogonPresence.Test.PubSub,
       secret_key_base: String.duplicate("a", 64),
       server: false}
    )

    start_supervised!({Presence, conn: conn})
    start_supervised!({DualPresence, conn: conn})
    start_supervised!({NatsReadPresence, conn: conn})
    start_supervised!(PhoenixPresence)
    :ok
  end

  for {name, module, topic_prefix} <- [
        {:nats, TrogonPresence.Test.Presence, "presence"},
        {:dual, TrogonPresence.Test.DualPresence, "presence:dual"},
        {:nats_read, TrogonPresence.Test.NatsReadPresence, "presence:nats_read"},
        {:phoenix, TrogonPresence.Test.PhoenixPresence, "presence:phoenix"}
      ] do
    test "the #{name} stage pushes presence_state then exactly one presence_diff per change" do
      module = unquote(module)
      identity = fresh_identity()
      topic = "#{unquote(topic_prefix)}:#{identity.key}"

      {:ok, _reply, _socket} =
        UserSocket
        |> socket("socket_id", %{})
        |> subscribe_and_join(topic, %{"user_id" => to_string(identity.key)})

      # Joining tracks the socket's own key, which is itself one change and
      # therefore one diff; it races the "presence_state" push and can land
      # on either side of it, so it is drained here rather than assumed away.
      assert_push("presence_diff", %{joins: self_join}, 5_000)
      assert Map.has_key?(self_join, to_string(identity.key))

      assert_push("presence_state", _initial_state, 5_000)
      refute_push("presence_diff", _anything, 200)

      other_key = "other-#{identity.key}"

      assert {:ok, _ref} = module.track(self(), topic, other_key, %{"status" => "away"})

      assert_push("presence_diff", %{joins: joins, leaves: leaves}, 5_000)
      assert Map.has_key?(joins, other_key)
      assert leaves == %{}
      refute_push("presence_diff", _anything, 200)

      :ok = module.untrack(self(), topic, other_key)

      assert_push("presence_diff", %{joins: %{}, leaves: left}, 5_000)
      assert Map.has_key?(left, other_key)
      refute_push("presence_diff", _anything, 200)
    end
  end
end
