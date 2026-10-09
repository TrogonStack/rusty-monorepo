defmodule TrogonPresence.MetaTest do
  use ExUnit.Case, async: true

  alias TrogonPresence.Meta

  test "accepts an ordinary map" do
    assert {:ok, meta} = Meta.new(%{"name" => "ana", "online" => true})
    assert Meta.to_map(meta) == %{"name" => "ana", "online" => true}
  end

  test "rejects reserved top level keys" do
    assert Meta.new(%{"phx_ref" => "x"}) == {:error, {:reserved_key, "phx_ref"}}
    assert Meta.new(%{"phx_ref_prev" => "x"}) == {:error, {:reserved_key, "phx_ref_prev"}}
  end

  test "rejects prototype keys at any depth" do
    assert Meta.new(%{"__proto__" => 1}) == {:error, {:reserved_key, "__proto__"}}
    assert Meta.new(%{"nested" => %{"constructor" => 1}}) == {:error, {:reserved_key, "constructor"}}
  end

  test "rejects a payload over the encoded size limit" do
    big = %{"blob" => String.duplicate("a", 5_000)}
    assert Meta.new(big) == {:error, :too_large}
  end

  test "rejects a non map" do
    assert Meta.new("not a map") == {:error, :not_a_map}
  end
end
