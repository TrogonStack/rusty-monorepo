defmodule TrogonPresence.OwnerEpoch do
  @moduledoc """
  The revision a shard or view lease was acquired at, plus who acquired it.
  Matches `trogon_presence::position::OwnerEpoch`. The shim only ever parses
  this out of wire headers; it never mints one.
  """

  alias TrogonPresence.{EntryRevision, OwnerId}

  @enforce_keys [:acquired, :owner]
  defstruct [:acquired, :owner]
  @type t :: %__MODULE__{acquired: EntryRevision.t(), owner: OwnerId.t()}

  @spec new(EntryRevision.t(), OwnerId.t()) :: t()
  def new(%EntryRevision{} = acquired, %OwnerId{} = owner) do
    %__MODULE__{acquired: acquired, owner: owner}
  end
end
