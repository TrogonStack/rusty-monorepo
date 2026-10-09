defmodule TrogonPresence.Wire.ReleaseReply do
  @moduledoc """
  A successful release reply. Matches
  `trogon_presence_service::writer::ReleaseReply`.
  """

  alias TrogonPresence.{MutationSequence, Topic}

  defmodule ReleasedEntry do
    @moduledoc """
    One released target's outcome. Matches
    `trogon_presence_service::writer::ReleasedReply`.
    """

    @enforce_keys [:topic, :lifetime, :status]
    defstruct [:topic, :lifetime, :status, :mutation_seq]

    @type t :: %__MODULE__{
            topic: Topic.t(),
            lifetime: TrogonPresence.LifetimeId.t(),
            status: :released | :gone,
            mutation_seq: MutationSequence.t() | nil
          }

    @spec decode(map()) :: t()
    def decode(%{"topic" => topic, "lifetime" => lifetime, "status" => status} = map) do
      %__MODULE__{
        topic: Topic.new!(topic),
        lifetime: TrogonPresence.LifetimeId.parse!(lifetime),
        status: decode_status(status),
        mutation_seq: decode_mutation_seq(map)
      }
    end

    defp decode_status("released"), do: :released
    defp decode_status("gone"), do: :gone

    defp decode_mutation_seq(%{"mutation_seq" => sequence}) when not is_nil(sequence),
      do: MutationSequence.parse!(sequence)

    defp decode_mutation_seq(_map), do: nil
  end

  @enforce_keys [:released, :holder_freed, :replayed]
  defstruct [:released, :holder_freed, :replayed]

  @type t :: %__MODULE__{
          released: [ReleasedEntry.t()],
          holder_freed: boolean(),
          replayed: boolean()
        }

  @spec decode(map()) :: t()
  def decode(%{"released" => released, "holder_freed" => holder_freed, "replayed" => replayed}) do
    %__MODULE__{
      released: Enum.map(released, &ReleasedEntry.decode/1),
      holder_freed: holder_freed,
      replayed: replayed
    }
  end
end
