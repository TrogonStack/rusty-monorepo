defmodule TrogonPresence.Snapshot.Assembly do
  @moduledoc """
  Reassembles a snapshot's parts into the `Presences` they encode, checking
  the manifest's advertised size, part count and digest along the way.
  Matches `trogon_presence_service::snapshot::Assembly`.
  """

  alias TrogonPresence.Presences
  alias TrogonPresence.Snapshot.{Digest, Frame, Limits, Manifest}

  @enforce_keys [:manifest, :parts, :received, :ended]
  defstruct [:manifest, :parts, :received, :ended]

  @type t :: %__MODULE__{
          manifest: Manifest.t(),
          parts: %{pos_integer() => binary()},
          received: non_neg_integer(),
          ended: boolean()
        }

  @type progress :: :pending | {:complete, Presences.t()}

  @type error ::
          :too_large
          | :part_count
          | :mixed_identity
          | :conflicting_duplicate
          | :out_of_range
          | :overflow
          | :truncated
          | :end_mismatch
          | :digest_mismatch
          | :decode

  @spec begin(Manifest.t(), Limits.t()) :: {:ok, t()} | {:error, error()}
  def begin(%Manifest{} = manifest, %Limits{} = limits) do
    cond do
      manifest.total_bytes > limits.max_bytes -> {:error, :too_large}
      manifest.parts > limits.max_parts -> {:error, :part_count}
      true -> {:ok, %__MODULE__{manifest: manifest, parts: %{}, received: 0, ended: false}}
    end
  end

  @spec accept(t(), Frame.t()) :: {:ok, t(), progress()} | {:error, error()}
  def accept(%__MODULE__{} = assembly, frame) do
    if Frame.identity(frame) != assembly.manifest.identity do
      {:error, :mixed_identity}
    else
      case apply_frame(assembly, frame) do
        {:ok, assembly} -> finish(assembly)
        {:error, reason} -> {:error, reason}
      end
    end
  end

  defp apply_frame(assembly, {:part, _identity, index, bytes}) do
    %{manifest: manifest, parts: parts, received: received} = assembly

    cond do
      index < 1 or index > manifest.parts ->
        {:error, :out_of_range}

      Map.get(parts, index, bytes) != bytes ->
        {:error, :conflicting_duplicate}

      Map.has_key?(parts, index) ->
        {:ok, assembly}

      true ->
        received = received + byte_size(bytes)

        if received > manifest.total_bytes do
          {:error, :overflow}
        else
          {:ok, %{assembly | parts: Map.put(parts, index, bytes), received: received}}
        end
    end
  end

  defp apply_frame(%{manifest: manifest} = assembly, {:end, _identity, parts}) do
    if parts != manifest.parts,
      do: {:error, :end_mismatch},
      else: {:ok, %{assembly | ended: true}}
  end

  defp finish(%{ended: false} = assembly), do: {:ok, assembly, :pending}

  defp finish(%{manifest: manifest, parts: parts} = assembly) do
    if map_size(parts) != manifest.parts do
      {:ok, assembly, :pending}
    else
      ordered = Enum.map(1..manifest.parts, &Map.fetch!(parts, &1))
      complete(assembly, ordered)
    end
  end

  defp complete(%{manifest: manifest, received: received} = assembly, ordered) do
    cond do
      received != manifest.total_bytes ->
        {:error, :truncated}

      Digest.of(ordered) != manifest.digest ->
        {:error, :digest_mismatch}

      true ->
        decode_body(assembly, IO.iodata_to_binary(ordered))
    end
  end

  defp decode_body(assembly, body) do
    with {:ok, wire} <- Jason.decode(body),
         {:ok, presences} <- Presences.decode(wire) do
      {:ok, assembly, {:complete, presences}}
    else
      _ -> {:error, :decode}
    end
  end
end
