defmodule TrogonPresence.PresenceParityTest do
  use TrogonPresence.Test.PresenceCase, async: false

  alias TrogonPresence.Test.{Presence, RealPresence}

  setup %{conn: conn} do
    start_supervised!({Phoenix.PubSub, name: TrogonPresence.Test.PubSub})
    start_supervised!({Presence, conn: conn})
    start_supervised!(RealPresence)
    :ok
  end

  test "the shim and real Phoenix.Presence agree on list/1 and get_by_key/2 for the same operations" do
    suffix = System.unique_integer([:positive, :monotonic])
    shim_topic = "presence:parity-shim:#{suffix}"
    real_topic = "presence:parity-real:#{suffix}"

    run_sequence(Presence, shim_topic)
    run_sequence(RealPresence, real_topic)

    expected = %{"alice" => [%{"status" => "busy"}]}

    wait_until(fn -> normalize_list(Presence.list(shim_topic)) == expected end)

    assert normalize_list(Presence.list(shim_topic)) == expected
    assert normalize_list(RealPresence.list(real_topic)) == expected

    assert normalize_list(Presence.list(shim_topic)) ==
             normalize_list(RealPresence.list(real_topic))

    assert normalize_entry(Presence.get_by_key(shim_topic, "alice")) ==
             normalize_entry(RealPresence.get_by_key(real_topic, "alice"))

    assert Presence.get_by_key(shim_topic, "bob") == []
    assert RealPresence.get_by_key(real_topic, "bob") == []
  end

  defp run_sequence(module, topic) do
    {:ok, _ref} = module.track(self(), topic, "alice", %{"status" => "online"})
    {:ok, _ref} = module.track(self(), topic, "bob", %{"status" => "away"})
    {:ok, _ref} = module.update(self(), topic, "alice", %{"status" => "busy"})

    {:ok, _ref} =
      module.update(self(), topic, "bob", fn meta -> Map.put(meta, "status", "online") end)

    :ok = module.untrack(self(), topic, "bob")
  end

  defp normalize_list(presences) do
    Map.new(presences, fn {key, %{metas: metas}} ->
      {to_string(key), Enum.map(metas, &Map.drop(&1, [:phx_ref, :phx_ref_prev]))}
    end)
  end

  defp normalize_entry([]), do: []

  defp normalize_entry(%{metas: metas}) do
    %{metas: Enum.map(metas, &Map.drop(&1, [:phx_ref, :phx_ref_prev]))}
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
