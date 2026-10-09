defmodule TrogonPresence.Test.PresenceChannel do
  @moduledoc """
  Mirrors the canonical `Phoenix.Presence` usage from its own moduledoc:
  track the joining process under the channel's `user_id` param, then push
  the topic's current state. Relies on `Phoenix.Channel`'s default
  `handle_out/3` (push the event through unchanged) to carry the
  `TrogonPresence.Tracker`-broadcast `"presence_diff"` to the client.

  One channel, several presence modules: the topic's second segment picks
  which `use TrogonPresence` stage a join tracks through (`"presence:alice"`
  keeps the plain `:nats` module every pre-A21.10 test already joins), so
  the same channel test shape can run against every stage without a module
  per stage.
  """

  use Phoenix.Channel

  alias TrogonPresence.Test.{DualPresence, NatsReadPresence, PhoenixPresence, Presence}

  @impl true
  def join("presence:dual:" <> _rest = topic, params, socket) do
    do_join(topic, params, socket, DualPresence)
  end

  def join("presence:nats_read:" <> _rest = topic, params, socket) do
    do_join(topic, params, socket, NatsReadPresence)
  end

  def join("presence:phoenix:" <> _rest = topic, params, socket) do
    do_join(topic, params, socket, PhoenixPresence)
  end

  def join("presence:" <> _rest = topic, params, socket) do
    do_join(topic, params, socket, Presence)
  end

  defp do_join(_topic, params, socket, module) do
    send(self(), :after_join)

    socket =
      socket
      |> assign(:user_id, Map.fetch!(params, "user_id"))
      |> assign(:presence_module, module)

    {:ok, socket}
  end

  @impl true
  def handle_info(:after_join, socket) do
    module = socket.assigns.presence_module
    {:ok, _ref} = module.track(socket, socket.assigns.user_id, %{"status" => "online"})
    push(socket, "presence_state", module.list(socket))
    {:noreply, socket}
  end
end
