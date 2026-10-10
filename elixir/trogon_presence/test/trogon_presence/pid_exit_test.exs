defmodule TrogonPresence.PidExitTest do
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

  test "a tracked pid's exit releases its entry: a leave diff reaches a channel client and list/1 drops the key" do
    identity = fresh_identity()
    topic = "presence:#{identity.key}"

    {:ok, _reply, _socket} =
      UserSocket
      |> socket("socket_id", %{})
      |> subscribe_and_join(topic, %{"user_id" => to_string(identity.key)})

    assert_push("presence_state", _initial_state, 5_000)

    other_key = "exiting-#{identity.key}"
    test_pid = self()

    holder_pid =
      spawn(fn ->
        {:ok, _ref} = Presence.track(self(), topic, other_key, %{"status" => "online"})
        send(test_pid, :tracked)

        receive do
          :stop -> :ok
        end
      end)

    assert_receive :tracked, 5_000
    assert_push("presence_diff", %{joins: %{^other_key => _joined}}, 5_000)

    ref = Process.monitor(holder_pid)
    send(holder_pid, :stop)
    assert_receive {:DOWN, ^ref, :process, ^holder_pid, _reason}, 5_000

    assert_push("presence_diff", %{joins: %{}, leaves: %{^other_key => _left}}, 5_000)

    refute Map.has_key?(Presence.list(topic), other_key)
  end
end
