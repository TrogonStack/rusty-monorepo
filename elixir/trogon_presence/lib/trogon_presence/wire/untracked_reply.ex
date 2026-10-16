defmodule TrogonPresence.Wire.UntrackedReply do
  @moduledoc """
  A successful untrack reply. Matches
  `trogon_presence_service::writer::UntrackedReply`.
  """

  @enforce_keys [:untracked]
  defstruct [:untracked]
  @type t :: %__MODULE__{untracked: boolean()}

  @spec decode(map()) :: t()
  def decode(%{"untracked" => untracked}), do: %__MODULE__{untracked: untracked}
end
