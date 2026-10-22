defmodule TrogonPresence.ViewCursor do
  @moduledoc """
  Where a reader is in a topic's diff log. Matches the `Service` variant of
  `trogon_presence::watch::ViewCursor`; the shim always reads through the
  presence service over NATS, so it never needs the `Local` variant the
  Rust type also carries for a purely in-process view.
  """

  alias TrogonPresence.{DiffSequence, GenerationEpoch}

  @enforce_keys [:epoch, :seq]
  defstruct [:epoch, :seq]
  @type t :: %__MODULE__{epoch: GenerationEpoch.t(), seq: DiffSequence.t()}

  @type step :: :next | :repeat | :gap | :stale | :rebase

  @spec new(GenerationEpoch.t(), DiffSequence.t()) :: t()
  def new(%GenerationEpoch{} = epoch, %DiffSequence{} = seq) do
    %__MODULE__{epoch: epoch, seq: seq}
  end

  @doc """
  Mirrors `ViewCursor::follow`: how `next` (announced with the sequence it
  claims to follow, `prev`) relates to this cursor.
  """
  @spec follow(t(), t(), DiffSequence.t()) :: step()
  def follow(%__MODULE__{} = cursor, %__MODULE__{} = next, %DiffSequence{} = prev) do
    case origin_order(cursor, next) do
      :older -> :rebase
      :conflict -> :rebase
      :newer -> :stale
      :same -> compare_seq(cursor.seq, next.seq, prev)
    end
  end

  defp origin_order(%__MODULE__{epoch: seen}, %__MODULE__{epoch: offered}) do
    case GenerationEpoch.compare(seen, offered) do
      {:ok, order} -> order
      {:error, :cross_generation} -> :conflict
    end
  end

  defp compare_seq(%{value: seen}, %{value: offered}, %{value: prev}) do
    cond do
      offered == seen and prev == seen -> :repeat
      offered <= seen -> :stale
      prev == seen -> :next
      true -> :gap
    end
  end
end
