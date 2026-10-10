defmodule TrogonPresence.Snapshot.Limits do
  @moduledoc """
  Receiver-side caps a reader enforces on an incoming snapshot, protecting
  it against a misbehaving or compromised service. Matches the receiving
  half of `trogon_presence_service::snapshot::SnapshotLimits`: the shim
  never assembles the sender-side payload-chunking or cross-connection
  admission budget, since it is never the one capturing and publishing a
  snapshot.
  """

  @enforce_keys [:max_bytes, :max_parts, :deadline_ms, :diff_buffer_bytes]
  defstruct [:max_bytes, :max_parts, :deadline_ms, :diff_buffer_bytes]

  @type t :: %__MODULE__{
          max_bytes: pos_integer(),
          max_parts: pos_integer(),
          deadline_ms: pos_integer(),
          diff_buffer_bytes: pos_integer()
        }

  @mib 1024 * 1024

  @spec default() :: t()
  def default do
    %__MODULE__{
      max_bytes: 8 * @mib,
      max_parts: 512,
      deadline_ms: 2_000,
      diff_buffer_bytes: 4 * @mib
    }
  end
end
