defmodule TrogonPresence.Shard do
  @moduledoc """
  A single shard index, unscoped as to whether it identifies a writer
  shard or a view shard. Matches `trogon_presence::shard::Shard`.
  """

  @enforce_keys [:index]
  defstruct [:index]
  @type t :: %__MODULE__{index: non_neg_integer()}

  @spec index(t()) :: non_neg_integer()
  def index(%__MODULE__{index: index}), do: index

  @doc false
  @spec new(non_neg_integer()) :: t()
  def new(index) when is_integer(index) and index >= 0, do: %__MODULE__{index: index}
end
