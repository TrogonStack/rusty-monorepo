defmodule TrogonPresence.Test.PresenceChannel do
  @moduledoc """
  Mirrors the canonical `Phoenix.Presence` usage from its own moduledoc:
  track the joining process under the channel's `user_id` param, then push
  the topic's current state. Relies on `Phoenix.Channel`'s default
  `handle_out/3` (push the event through unchanged) to carry the
  `TrogonPresence.Tracker`-broadcast `"presence_diff"` to the client.
  """

  use Phoenix.Channel

  alias TrogonPresence.Test.Presence

  @impl true
  def join("presence:" <> _rest, params, socket) do
    send(self(), :after_join)
    {:ok, assign(socket, :user_id, Map.fetch!(params, "user_id"))}
  end

  @impl true
  def handle_info(:after_join, socket) do
    {:ok, _ref} = Presence.track(socket, socket.assigns.user_id, %{"status" => "online"})
    push(socket, "presence_state", Presence.list(socket))
    {:noreply, socket}
  end
end
