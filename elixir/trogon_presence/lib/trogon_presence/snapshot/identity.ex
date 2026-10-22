defmodule TrogonPresence.Snapshot.Identity do
  @moduledoc """
  The request, snapshot, epoch and sequence a snapshot manifest and its
  frames all share. Matches
  `trogon_presence_service::snapshot::SnapshotIdentity`.
  """

  alias TrogonPresence.{
    DiffSequence,
    EntryRevision,
    GenerationEpoch,
    OwnerEpoch,
    OwnerId,
    RequestId,
    SnapshotId,
    StreamGeneration,
    ViewCursor
  }

  @enforce_keys [:request, :snapshot, :epoch, :seq]
  defstruct [:request, :snapshot, :epoch, :seq]

  @type t :: %__MODULE__{
          request: RequestId.t(),
          snapshot: SnapshotId.t(),
          epoch: GenerationEpoch.t(),
          seq: DiffSequence.t()
        }

  @header_generation "presence-generation"
  @header_owner_rev "presence-owner-rev"
  @header_owner_id "presence-owner-id"
  @header_seq "presence-seq"
  @header_snapshot_id "presence-snapshot-id"
  @header_request_id "presence-request-id"

  @spec cursor(t()) :: ViewCursor.t()
  def cursor(%__MODULE__{epoch: epoch, seq: seq}), do: ViewCursor.new(epoch, seq)

  @spec from_headers([{binary(), binary()}]) :: {:ok, t()} | :error
  def from_headers(headers) when is_list(headers) do
    with {:ok, generation} <- header(headers, @header_generation, &StreamGeneration.parse/1),
         {:ok, owner} <- header(headers, @header_owner_id, &OwnerId.parse/1),
         {:ok, acquired} <- header(headers, @header_owner_rev, &EntryRevision.parse/1),
         {:ok, seq} <- header(headers, @header_seq, &DiffSequence.parse/1),
         {:ok, snapshot} <- header(headers, @header_snapshot_id, &SnapshotId.parse/1),
         {:ok, request} <- header(headers, @header_request_id, &RequestId.parse/1) do
      epoch = GenerationEpoch.new(generation, OwnerEpoch.new(acquired, owner))
      {:ok, %__MODULE__{request: request, snapshot: snapshot, epoch: epoch, seq: seq}}
    else
      _ -> :error
    end
  end

  def from_headers(_headers), do: :error

  defp header(headers, name, parse) do
    case List.keyfind(headers, name, 0) do
      {^name, value} -> parse.(to_string(value))
      nil -> :error
    end
  end
end
