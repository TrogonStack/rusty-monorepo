defmodule TrogonPresence.Tracker.Tracked do
  @moduledoc """
  One local `track/4` registration: the pid that called it, the identity it
  was written under, and the write-side bookkeeping (`holder`, `lifetime`,
  `mutation_seq`) needed to heartbeat, update or untrack it later without
  asking the service for it again.
  """

  alias TrogonPresence.{HolderId, Key, LifetimeId, Meta, MutationSequence, StoredRef, Topic}

  @enforce_keys [:pid, :topic, :key, :holder, :lifetime, :mutation_seq, :meta, :phx_ref]
  defstruct [:pid, :topic, :key, :holder, :lifetime, :mutation_seq, :meta, :phx_ref]

  @type t :: %__MODULE__{
          pid: pid(),
          topic: Topic.t(),
          key: Key.t(),
          holder: HolderId.t(),
          lifetime: LifetimeId.t(),
          mutation_seq: MutationSequence.t(),
          meta: Meta.t(),
          phx_ref: StoredRef.t()
        }
end
