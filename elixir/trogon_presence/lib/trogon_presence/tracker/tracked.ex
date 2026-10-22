defmodule TrogonPresence.Tracker.Tracked do
  @moduledoc """
  One local `track/4` registration: the pid that called it, the identity it
  was written under, and the write-side bookkeeping (`holder`, `lifetime`,
  `mutation_seq`) needed to heartbeat, update or untrack it later without
  asking the service for it again.

  `:nats` fills every field. `:dual` and `:nats_read` also write through
  Phoenix first, so `topic`, `key`, `holder`, `lifetime` and `mutation_seq`
  only exist once the NATS mirror write has actually succeeded; `mirrored?`
  says whether that happened, and `topic_raw`/`key_raw` stay available either
  way so a failed mirror can be retried later without the original pid
  calling back in.
  """

  alias TrogonPresence.{HolderId, Key, LifetimeId, Meta, MutationSequence, StoredRef, Topic}

  @enforce_keys [:pid, :topic_raw, :key_raw, :meta, :phx_ref, :mirrored?]
  defstruct [
    :pid,
    :topic_raw,
    :key_raw,
    :meta,
    :phx_ref,
    :mirrored?,
    :topic,
    :key,
    :holder,
    :lifetime,
    :mutation_seq
  ]

  @type t :: %__MODULE__{
          pid: pid(),
          topic_raw: String.t(),
          key_raw: String.t(),
          meta: Meta.t() | nil,
          phx_ref: StoredRef.t() | nil,
          mirrored?: boolean(),
          topic: Topic.t() | nil,
          key: Key.t() | nil,
          holder: HolderId.t() | nil,
          lifetime: LifetimeId.t() | nil,
          mutation_seq: MutationSequence.t() | nil
        }
end
