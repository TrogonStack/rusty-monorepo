defmodule TrogonPresence.OpaqueIdTest do
  use ExUnit.Case, async: true

  alias TrogonPresence.HolderId

  test "generates a 22 character canonical id" do
    id = HolderId.generate()
    assert String.length(HolderId.to_string(id)) == 22
  end

  test "round trips through its string form" do
    id = HolderId.generate()
    encoded = HolderId.to_string(id)
    assert {:ok, decoded} = HolderId.parse(encoded)
    assert decoded == id
  end

  test "rejects a non canonical string" do
    assert HolderId.parse("not-an-id") == :error
    assert HolderId.parse("") == :error
  end

  test "rejects a string that decodes but does not round trip" do
    id = HolderId.generate()
    encoded = HolderId.to_string(id)
    flipped = String.replace_suffix(encoded, String.last(encoded), "=")
    assert HolderId.parse(flipped) == :error
  end
end
