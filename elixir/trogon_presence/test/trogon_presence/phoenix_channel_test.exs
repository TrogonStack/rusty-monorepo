defmodule TrogonPresence.PhoenixChannelTest do
  use TrogonPresence.Test.PresenceCase, async: false
  use Phoenix.ChannelTest

  @endpoint TrogonPresence.Test.Endpoint

  alias TrogonPresence.Test.{Presence, UserSocket}

  setup %{conn: conn} do
    start_supervised!({Phoenix.PubSub, name: TrogonPresence.Test.PubSub})

    start_supervised!(
      {TrogonPresence.Test.Endpoint,
       pubsub_server: TrogonPresence.Test.PubSub,
       secret_key_base: String.duplicate("a", 64),
       server: false}
    )

    start_supervised!({Presence, conn: conn})
    :ok
  end

  test "a channel client receives a presence_state push, then a presence_diff broadcast for another holder" do
    identity = fresh_identity()
    topic = "presence:#{identity.key}"

    {:ok, _reply, _socket} =
      UserSocket
      |> socket("socket_id", %{})
      |> subscribe_and_join(topic, %{"user_id" => to_string(identity.key)})

    assert_push("presence_state", _initial_state, 5_000)

    other_key = "other-#{identity.key}"

    assert {:ok, _ref} =
             Presence.track(self(), topic, other_key, %{"status" => "away"})

    assert_push("presence_diff", %{joins: joins, leaves: leaves}, 5_000)
    assert Map.has_key?(joins, other_key)
    assert leaves == %{}

    :ok = Presence.untrack(self(), topic, other_key)

    assert_push("presence_diff", %{joins: %{}, leaves: left}, 5_000)
    assert Map.has_key?(left, other_key)
  end
end
