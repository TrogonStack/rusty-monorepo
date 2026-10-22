defmodule TrogonPresence.CodecTest do
  use ExUnit.Case, async: true

  alias TrogonPresence.Codec

  test "passes alphanumerics, underscore and hyphen through unchanged" do
    assert Codec.encode("room-lobby_42") == "room-lobby_42"
  end

  test "escapes bytes outside the alphabet as uppercase hex" do
    assert Codec.encode("ana@x.io") == "ana=40x=2Eio"
    assert Codec.encode("=") == "=3D"
  end

  test "encodes the empty string to a literal equals sign" do
    assert Codec.encode("") == "="
  end

  test "round trips arbitrary utf8" do
    for raw <- ["room:lobby", "josé", "", "=", "a.b:c", "plain_token-1"] do
      assert {:ok, ^raw} = raw |> Codec.encode() |> Codec.decode()
    end
  end

  test "rejects non canonical escapes" do
    assert Codec.decode("=3d") == :error
    assert Codec.decode("=4") == :error
    assert Codec.decode("=5A") == :error
    assert Codec.decode("==") == :error
  end
end
