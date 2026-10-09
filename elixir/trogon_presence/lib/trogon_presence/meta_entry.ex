defmodule TrogonPresence.MetaEntry do
  @moduledoc """
  One reader-visible presence entry. Matches
  `trogon_presence::watch::presences::MetaEntry`: the meta's own fields
  flattened together with `phx_ref` and an optional `phx_ref_prev` into one
  JSON object, which is exactly the shape `Phoenix.Presence` itself uses.
  """

  alias TrogonPresence.{Meta, ViewRef}

  @enforce_keys [:phx_ref, :meta]
  defstruct [:phx_ref, :phx_ref_prev, :meta]

  @type t :: %__MODULE__{
          phx_ref: ViewRef.t(),
          phx_ref_prev: ViewRef.t() | nil,
          meta: Meta.t()
        }

  @spec new(ViewRef.t(), ViewRef.t() | nil, Meta.t()) :: t()
  def new(%ViewRef{} = phx_ref, phx_ref_prev, %Meta{} = meta) do
    %__MODULE__{phx_ref: phx_ref, phx_ref_prev: phx_ref_prev, meta: meta}
  end

  @spec decode(map()) :: {:ok, t()} | :error
  def decode(%{"phx_ref" => phx_ref} = fields) when is_binary(phx_ref) do
    with {:ok, phx_ref} <- ViewRef.new(phx_ref),
         {:ok, phx_ref_prev} <- decode_prev(fields["phx_ref_prev"]),
         rest = fields |> Map.delete("phx_ref") |> Map.delete("phx_ref_prev"),
         {:ok, meta} <- Meta.new(rest) do
      {:ok, new(phx_ref, phx_ref_prev, meta)}
    else
      _ -> :error
    end
  end

  def decode(_fields), do: :error

  defp decode_prev(nil), do: {:ok, nil}

  defp decode_prev(value) when is_binary(value) do
    case ViewRef.new(value) do
      {:ok, ref} -> {:ok, ref}
      {:error, _reason} -> :error
    end
  end

  defp decode_prev(_value), do: :error

  @spec encode(t()) :: map()
  def encode(%__MODULE__{phx_ref: phx_ref, phx_ref_prev: phx_ref_prev, meta: meta}) do
    meta
    |> Meta.to_map()
    |> Map.put("phx_ref", ViewRef.to_string(phx_ref))
    |> maybe_put_prev(phx_ref_prev)
  end

  defp maybe_put_prev(wire, nil), do: wire

  defp maybe_put_prev(wire, %ViewRef{} = prev),
    do: Map.put(wire, "phx_ref_prev", ViewRef.to_string(prev))
end
