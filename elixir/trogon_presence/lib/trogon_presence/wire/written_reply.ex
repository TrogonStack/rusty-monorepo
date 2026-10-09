defmodule TrogonPresence.Wire.WrittenReply do
  @moduledoc """
  A successful track/update reply. Matches
  `trogon_presence_service::writer::WrittenReply`.
  """

  alias TrogonPresence.{EntryRevision, LifetimeId, MutationSequence, StoredRef}

  @enforce_keys [:phx_ref, :rev, :lifetime, :mutation_seq, :adopted]
  defstruct [:phx_ref, :rev, :lifetime, :mutation_seq, :adopted]

  @type t :: %__MODULE__{
          phx_ref: StoredRef.t(),
          rev: EntryRevision.t(),
          lifetime: LifetimeId.t(),
          mutation_seq: MutationSequence.t(),
          adopted: boolean()
        }

  @spec decode(map()) :: t()
  def decode(%{
        "phx_ref" => phx_ref,
        "rev" => rev,
        "lifetime" => lifetime,
        "mutation_seq" => mutation_seq,
        "adopted" => adopted
      }) do
    %__MODULE__{
      phx_ref: StoredRef.new!(phx_ref),
      rev: EntryRevision.parse!(rev),
      lifetime: LifetimeId.parse!(lifetime),
      mutation_seq: MutationSequence.parse!(mutation_seq),
      adopted: adopted
    }
  end
end
