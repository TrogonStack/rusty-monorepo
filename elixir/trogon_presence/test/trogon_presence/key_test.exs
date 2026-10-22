defmodule TrogonPresence.KeyTest do
  use ExUnit.Case, async: true

  alias TrogonPresence.Key

  test "tokenizes the whole string as one unit" do
    assert {:ok, key} = Key.new("ana@x.io")
    assert Key.token(key) == "ana=40x=2Eio"
    assert Key.raw(key) == "ana@x.io"
  end

  test "rejects the empty key" do
    assert Key.new("") == {:error, :empty}
  end

  test "rejects a key past the raw byte limit" do
    assert Key.new(String.duplicate("a", 257)) == {:error, :too_long}
  end

  test "round trips through its token" do
    assert {:ok, key} = Key.new("ana@x.io")
    assert {:ok, decoded} = Key.from_token(Key.token(key))
    assert decoded == key
  end
end
