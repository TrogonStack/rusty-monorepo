defmodule TrogonPresence.RetryBackoff do
  @moduledoc """
  How long a reader waits before retrying a refused or abandoned snapshot
  request: doubling from a floor up to a ceiling, jittered so that many
  readers recovering at once do not retry in lockstep. Matches
  `trogon_presence::watch::replay::RetryBackoff`.
  """

  @enforce_keys [:ms]
  defstruct [:ms]
  @type t :: %__MODULE__{ms: pos_integer()}

  @min_ms 100
  @max_ms 5_000

  @spec default() :: t()
  def default, do: %__MODULE__{ms: @min_ms}

  @spec next(t()) :: t()
  def next(%__MODULE__{ms: ms}), do: %__MODULE__{ms: min(ms * 2, @max_ms)}

  @doc "Mirrors `RetryBackoff::jittered`: the current delay plus jitter in `[0, ms)`."
  @spec jittered_ms(t()) :: pos_integer()
  def jittered_ms(%__MODULE__{ms: ms}) do
    ceiling = max(ms, 1)
    ms + :rand.uniform(ceiling) - 1
  end
end
