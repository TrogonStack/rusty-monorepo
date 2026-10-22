defmodule TrogonPresence.Snapshot.Manifest do
  @moduledoc """
  Describes a snapshot before its parts arrive: how many parts, how many
  total bytes, and the digest they must hash to. Matches
  `trogon_presence_service::snapshot::SnapshotManifest`.
  """

  alias TrogonPresence.Snapshot.{Digest, Identity}

  alias TrogonPresence.{
    DiffSequence,
    EntryRevision,
    GenerationEpoch,
    OwnerEpoch,
    OwnerId,
    RequestId,
    SnapshotId,
    StreamGeneration
  }

  @enforce_keys [:identity, :total_bytes, :parts, :digest]
  defstruct [:identity, :total_bytes, :parts, :digest]

  @type t :: %__MODULE__{
          identity: Identity.t(),
          total_bytes: non_neg_integer(),
          parts: pos_integer(),
          digest: Digest.t()
        }

  @spec identity(t()) :: Identity.t()
  def identity(%__MODULE__{identity: identity}), do: identity

  @spec decode(map()) :: {:ok, t()} | :error
  def decode(%{
        "request_id" => request_id,
        "snapshot_id" => snapshot_id,
        "generation" => generation,
        "owner_epoch" => %{"acquired" => acquired, "owner" => owner},
        "seq" => seq,
        "total_bytes" => total_bytes,
        "parts" => parts,
        "sha256" => sha256
      })
      when is_integer(total_bytes) and total_bytes >= 0 and is_integer(parts) and parts > 0 do
    with {:ok, request} <- RequestId.parse(request_id),
         {:ok, snapshot} <- SnapshotId.parse(snapshot_id),
         {:ok, generation} <- StreamGeneration.parse(generation),
         {:ok, owner} <- OwnerId.parse(owner),
         {:ok, acquired} <- EntryRevision.parse(acquired),
         {:ok, seq} <- DiffSequence.parse(seq),
         {:ok, digest} <- Digest.parse(sha256) do
      epoch = GenerationEpoch.new(generation, OwnerEpoch.new(acquired, owner))
      identity = %Identity{request: request, snapshot: snapshot, epoch: epoch, seq: seq}

      {:ok,
       %__MODULE__{identity: identity, total_bytes: total_bytes, parts: parts, digest: digest}}
    else
      _ -> :error
    end
  end

  def decode(_wire), do: :error
end
