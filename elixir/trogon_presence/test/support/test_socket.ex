defmodule TrogonPresence.Test.UserSocket do
  @moduledoc false

  use Phoenix.Socket

  channel("presence:*", TrogonPresence.Test.PresenceChannel)

  @impl true
  def connect(_params, socket, _connect_info), do: {:ok, socket}

  @impl true
  def id(_socket), do: nil
end
