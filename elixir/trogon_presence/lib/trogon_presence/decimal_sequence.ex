defmodule TrogonPresence.DecimalSequence do
  @moduledoc """
  Shared shape for the protocol's canonical-decimal-string u64 sequence
  types (`MutationSequence`, `DiffSequence`, `EntryRevision`). Matches the
  `decimal_u64` serde module in `trogon_presence::position`: the wire form is
  a JSON string of ASCII digits, `"0"` or with no leading zero.

  `use TrogonPresence.DecimalSequence` to define a distinct Value Object
  instead of passing a bare integer or string around.
  """

  @max_u64 0xFFFFFFFFFFFFFFFF

  defmacro __using__(_opts) do
    quote do
      @enforce_keys [:value]
      defstruct [:value]
      @type t :: %__MODULE__{value: non_neg_integer()}

      @spec from_integer(non_neg_integer()) :: t()
      def from_integer(value),
        do: %__MODULE__{value: TrogonPresence.DecimalSequence.validate!(value)}

      @spec parse(binary()) :: {:ok, t()} | :error
      def parse(text) do
        case TrogonPresence.DecimalSequence.parse(text) do
          {:ok, value} -> {:ok, %__MODULE__{value: value}}
          :error -> :error
        end
      end

      @spec parse!(binary()) :: t()
      def parse!(text) do
        case parse(text) do
          {:ok, sequence} -> sequence
          :error -> raise ArgumentError, "invalid #{inspect(__MODULE__)}: #{inspect(text)}"
        end
      end

      @spec to_string(t()) :: binary()
      def to_string(%__MODULE__{value: value}), do: Integer.to_string(value)

      @spec compare(t(), t()) :: :lt | :eq | :gt
      def compare(%__MODULE__{value: a}, %__MODULE__{value: b}),
        do: TrogonPresence.DecimalSequence.compare(a, b)

      defimpl String.Chars do
        def to_string(sequence), do: @for.to_string(sequence)
      end

      defimpl Jason.Encoder do
        def encode(sequence, opts), do: Jason.Encode.string(@for.to_string(sequence), opts)
      end
    end
  end

  @spec validate!(non_neg_integer()) :: non_neg_integer()
  def validate!(value) when is_integer(value) and value >= 0 and value <= @max_u64, do: value

  @spec parse(binary()) :: {:ok, non_neg_integer()} | :error
  def parse("0"), do: {:ok, 0}

  def parse(text) when is_binary(text) do
    with true <- text != "",
         true <- :binary.first(text) != ?0,
         true <- String.match?(text, ~r/^[0-9]+$/),
         {value, ""} <- Integer.parse(text),
         true <- value <= @max_u64 do
      {:ok, value}
    else
      _ -> :error
    end
  end

  def parse(_), do: :error

  @spec compare(non_neg_integer(), non_neg_integer()) :: :lt | :eq | :gt
  def compare(a, b) when a < b, do: :lt
  def compare(a, b) when a > b, do: :gt
  def compare(_a, _b), do: :eq
end
