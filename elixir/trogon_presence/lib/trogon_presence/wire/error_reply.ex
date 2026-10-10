defmodule TrogonPresence.Wire.ErrorReply do
  @moduledoc """
  A decoded presence service error reply. Matches
  `trogon_presence_service::reply::ErrorReply`'s wire shape. `code` keeps the
  server's reply code string (`"gone"`, `"conflict"`, `"not_owner"`, ...)
  verbatim rather than mapping it onto a closed Elixir enum, since the set of
  codes belongs to the service and a client pinned to an older shim should
  not crash decoding a code introduced after it shipped.
  """

  alias TrogonPresence.{LifetimeId, Meta, MutationSequence, StoredRef}

  @enforce_keys [:code, :retryable, :outcome_unknown]
  defstruct [
    :code,
    :detail,
    :shard,
    :phx_ref,
    :meta,
    :lifetime,
    :mutation_seq,
    :retryable,
    :outcome_unknown
  ]

  @type t :: %__MODULE__{
          code: binary(),
          detail: binary() | nil,
          shard: binary() | nil,
          phx_ref: StoredRef.t() | nil,
          meta: Meta.t() | nil,
          lifetime: LifetimeId.t() | nil,
          mutation_seq: MutationSequence.t() | nil,
          retryable: boolean(),
          outcome_unknown: boolean()
        }

  @spec decode(map()) :: t()
  def decode(%{"error" => code} = map) do
    %__MODULE__{
      code: code,
      detail: Map.get(map, "detail"),
      shard: Map.get(map, "shard"),
      phx_ref: decode_phx_ref(map),
      meta: decode_meta(map),
      lifetime: decode_lifetime(map),
      mutation_seq: decode_mutation_seq(map),
      retryable: Map.fetch!(map, "retryable"),
      outcome_unknown: Map.fetch!(map, "outcome_unknown")
    }
  end

  @spec retryable?(t()) :: boolean()
  def retryable?(%__MODULE__{retryable: retryable}), do: retryable

  @spec outcome_unknown?(t()) :: boolean()
  def outcome_unknown?(%__MODULE__{outcome_unknown: outcome_unknown}), do: outcome_unknown

  defp decode_phx_ref(%{"phx_ref" => ref}), do: StoredRef.new!(ref)
  defp decode_phx_ref(_map), do: nil

  defp decode_meta(%{"meta" => meta}) when is_map(meta), do: Meta.new!(meta)
  defp decode_meta(_map), do: nil

  defp decode_lifetime(%{"lifetime" => lifetime}), do: LifetimeId.parse!(lifetime)
  defp decode_lifetime(_map), do: nil

  defp decode_mutation_seq(%{"mutation_seq" => sequence}), do: MutationSequence.parse!(sequence)
  defp decode_mutation_seq(_map), do: nil
end
