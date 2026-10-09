defmodule TrogonPresence.OpaqueId do
  @moduledoc """
  Shared shape for the protocol's opaque identifiers: 16 random bytes encoded
  as a 22 character unpadded base64url string, matching every
  `random_id_bytes()`/`encode_id()` opaque id in `trogon_presence::position`.

  `use TrogonPresence.OpaqueId` to define a distinct Value Object type instead
  of passing the encoded string around as a bare binary.
  """

  @id_bytes 16

  defmacro __using__(_opts) do
    quote do
      @enforce_keys [:bytes]
      defstruct [:bytes]
      @type t :: %__MODULE__{bytes: <<_::128>>}

      @spec generate() :: t()
      def generate do
        %__MODULE__{bytes: :crypto.strong_rand_bytes(unquote(TrogonPresence.OpaqueId.id_bytes()))}
      end

      @spec from_bytes(binary()) :: t()
      def from_bytes(bytes)
          when is_binary(bytes) and
                 byte_size(bytes) == unquote(TrogonPresence.OpaqueId.id_bytes()) do
        %__MODULE__{bytes: bytes}
      end

      @spec parse(binary()) :: {:ok, t()} | :error
      def parse(encoded), do: TrogonPresence.OpaqueId.parse(__MODULE__, encoded)

      @spec parse!(binary()) :: t()
      def parse!(encoded) do
        case parse(encoded) do
          {:ok, id} -> id
          :error -> raise ArgumentError, "invalid #{inspect(__MODULE__)}: #{inspect(encoded)}"
        end
      end

      @spec to_string(t()) :: binary()
      def to_string(%__MODULE__{bytes: bytes}), do: TrogonPresence.OpaqueId.encode(bytes)

      defimpl String.Chars do
        def to_string(id), do: @for.to_string(id)
      end

      defimpl Jason.Encoder do
        def encode(id, opts), do: Jason.Encode.string(@for.to_string(id), opts)
      end

      defimpl Inspect do
        def inspect(id, _opts) do
          "#" <> inspect_name(@for) <> "<" <> @for.to_string(id) <> ">"
        end

        defp inspect_name(module), do: module |> Module.split() |> List.last()
      end
    end
  end

  @spec id_bytes() :: pos_integer()
  def id_bytes, do: @id_bytes

  @spec encode(binary()) :: binary()
  def encode(bytes), do: Base.url_encode64(bytes, padding: false)

  @spec parse(module(), binary()) :: {:ok, struct()} | :error
  def parse(module, encoded) when is_binary(encoded) do
    with {:ok, bytes} <- Base.url_decode64(encoded, padding: false),
         true <- byte_size(bytes) == @id_bytes,
         true <- encode(bytes) == encoded do
      {:ok, struct!(module, bytes: bytes)}
    else
      _ -> :error
    end
  end
end
