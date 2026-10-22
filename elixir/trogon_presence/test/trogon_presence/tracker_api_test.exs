defmodule TrogonPresence.TrackerApiTest do
  use TrogonPresence.Test.PresenceCase, async: false

  alias TrogonPresence.Test.Presence

  setup %{conn: conn} do
    start_supervised!({Phoenix.PubSub, name: TrogonPresence.Test.PubSub})
    start_supervised!({Presence, conn: conn})
    :ok
  end

  test "list/1 is empty for a never-tracked topic and reflects a tracked entry once synced" do
    identity = fresh_identity()
    topic = "presence:#{identity.key}"
    key = "alice-#{identity.key}"

    assert Presence.list(topic) == %{}

    assert {:ok, _ref} = Presence.track(self(), topic, key, %{"status" => "online"})

    wait_until(fn -> Map.has_key?(Presence.list(topic), key) end)
    assert %{^key => %{metas: [%{"status" => "online"}]}} = Presence.list(topic)
  end

  test "get_by_key/2 returns the entry when present and [] when absent" do
    identity = fresh_identity()
    topic = "presence:#{identity.key}"
    key = "bob-#{identity.key}"

    assert Presence.get_by_key(topic, key) == []

    assert {:ok, _ref} = Presence.track(self(), topic, key, %{"status" => "online"})

    wait_until(fn -> Presence.get_by_key(topic, key) != [] end)
    assert %{metas: [%{"status" => "online"}]} = Presence.get_by_key(topic, key)
    assert Presence.get_by_key(topic, "missing-#{identity.key}") == []
  end

  test "update/4 accepts a map and the new meta is what gets read back" do
    identity = fresh_identity()
    topic = "presence:#{identity.key}"
    key = "carol-#{identity.key}"

    {:ok, _ref} = Presence.track(self(), topic, key, %{"status" => "online"})
    assert {:ok, _ref} = Presence.update(self(), topic, key, %{"status" => "busy"})

    wait_until(fn ->
      match?(%{metas: [%{"status" => "busy"}]}, Presence.get_by_key(topic, key))
    end)

    assert %{metas: [%{"status" => "busy"}]} = Presence.get_by_key(topic, key)
  end

  test "update/4 accepts a function that receives the current meta" do
    identity = fresh_identity()
    topic = "presence:#{identity.key}"
    key = "dave-#{identity.key}"

    {:ok, _ref} = Presence.track(self(), topic, key, %{"status" => "online", "count" => 0})

    assert {:ok, _ref} =
             Presence.update(self(), topic, key, fn meta ->
               Map.update!(meta, "count", &(&1 + 1))
             end)

    wait_until(fn ->
      match?(%{metas: [%{"count" => 1}]}, Presence.get_by_key(topic, key))
    end)

    assert %{metas: [%{"status" => "online", "count" => 1}]} = Presence.get_by_key(topic, key)
  end

  test "update/4 on an untracked key returns the same error shape Phoenix.Tracker returns" do
    identity = fresh_identity()
    topic = "presence:#{identity.key}"
    key = "erin-#{identity.key}"

    assert Presence.update(self(), topic, key, %{"status" => "online"}) == {:error, :nopresence}
  end

  test "track/4 on an already-tracked {pid, topic, key} returns the same error shape Phoenix.Tracker returns" do
    identity = fresh_identity()
    topic = "presence:#{identity.key}"
    key = "frank-#{identity.key}"

    {:ok, _ref} = Presence.track(self(), topic, key, %{"status" => "online"})

    this = self()

    assert Presence.track(self(), topic, key, %{"status" => "online"}) ==
             {:error, {:already_tracked, this, topic, key}}
  end

  test "untrack/4 on a never-tracked key is always :ok, matching Phoenix.Tracker" do
    identity = fresh_identity()
    topic = "presence:#{identity.key}"
    key = "grace-#{identity.key}"

    assert Presence.untrack(self(), topic, key) == :ok
  end

  test "untrack/4 removes a tracked entry from list/1" do
    identity = fresh_identity()
    topic = "presence:#{identity.key}"
    key = "henry-#{identity.key}"

    {:ok, _ref} = Presence.track(self(), topic, key, %{"status" => "online"})
    wait_until(fn -> Map.has_key?(Presence.list(topic), key) end)

    assert Presence.untrack(self(), topic, key) == :ok

    wait_until(fn -> Presence.list(topic) == %{} end)
    assert Presence.list(topic) == %{}
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
