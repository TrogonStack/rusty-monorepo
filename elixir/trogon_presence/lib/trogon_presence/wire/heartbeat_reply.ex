defmodule TrogonPresence.Wire.HeartbeatReply do
  @moduledoc """
  A successful heartbeat reply. Matches
  `trogon_presence_service::writer::HeartbeatReply`. `entries` holds one
  `BeatStatus` wire string per entry submitted, in the same order.
  """

  @enforce_keys [:interval, :entries]
  defstruct [:interval, :entries]
  @type t :: %__MODULE__{interval: non_neg_integer(), entries: [binary()]}

  @spec decode(map()) :: t()
  def decode(%{"interval" => interval, "entries" => entries}) do
    %__MODULE__{interval: interval, entries: entries}
  end
end
