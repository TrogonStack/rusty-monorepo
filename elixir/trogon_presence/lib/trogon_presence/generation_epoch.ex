defmodule TrogonPresence.GenerationEpoch do
  @moduledoc """
  The owner epoch of one generation of the presence service's backing
  stream. Matches `trogon_presence::position::GenerationEpoch`. The shim
  only ever parses this out of wire headers; it never mints one.
  """

  alias TrogonPresence.{OwnerEpoch, StreamGeneration}

  @enforce_keys [:generation, :epoch]
  defstruct [:generation, :epoch]
  @type t :: %__MODULE__{generation: StreamGeneration.t(), epoch: OwnerEpoch.t()}

  @type order :: :older | :same | :newer | :conflict

  @spec new(StreamGeneration.t(), OwnerEpoch.t()) :: t()
  def new(%StreamGeneration{} = generation, %OwnerEpoch{} = epoch) do
    %__MODULE__{generation: generation, epoch: epoch}
  end

  @doc """
  Compares two epochs from the same generation. Mirrors
  `GenerationEpoch::compare`: an error when the generations differ (they are
  not comparable at all), otherwise the acquired revision order, with equal
  revisions from different owners reported as `:conflict`.
  """
  @spec compare(t(), t()) :: {:ok, order()} | {:error, :cross_generation}
  def compare(
        %__MODULE__{generation: %{bytes: same}} = left,
        %__MODULE__{generation: %{bytes: same}} = right
      ) do
    {:ok, revision_order(left.epoch, right.epoch)}
  end

  def compare(%__MODULE__{}, %__MODULE__{}), do: {:error, :cross_generation}

  defp revision_order(%OwnerEpoch{acquired: %{value: a}, owner: owner_a}, %OwnerEpoch{
         acquired: %{value: b},
         owner: owner_b
       }) do
    cond do
      a < b -> :older
      a > b -> :newer
      owner_a == owner_b -> :same
      true -> :conflict
    end
  end
end
