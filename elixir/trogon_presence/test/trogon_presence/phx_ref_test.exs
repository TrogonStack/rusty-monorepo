defmodule TrogonPresence.PhxRefTest do
  use ExUnit.Case, async: true

  alias TrogonPresence.{StoredRef, ViewRef}

  test "accepts short foreign refs that are not the canonical opaque id shape" do
    assert {:ok, ref} = StoredRef.new("F1a2b3c4d5e6")
    assert StoredRef.to_string(ref) == "F1a2b3c4d5e6"
    assert {:ok, _} = ViewRef.new("abc")
  end

  test "accepts a ref at the 64 byte limit and rejects one past it" do
    assert {:ok, _} = StoredRef.new(String.duplicate("x", 64))
    assert StoredRef.new(String.duplicate("x", 65)) == {:error, :too_long}
  end

  test "rejects the empty ref" do
    assert StoredRef.new("") == {:error, :empty}
    assert ViewRef.new("") == {:error, :empty}
  end
end
