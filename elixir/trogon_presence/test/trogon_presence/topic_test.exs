defmodule TrogonPresence.TopicTest do
  use ExUnit.Case, async: true

  alias TrogonPresence.Topic

  test "tokenizes simple segments" do
    assert {:ok, topic} = Topic.new("room:lobby")
    assert Topic.tokens(topic) == "room.lobby"
    assert Topic.raw(topic) == "room:lobby"
  end

  test "escapes non ascii segments" do
    assert {:ok, topic} = Topic.new("room:josé")
    assert Topic.tokens(topic) == "room.jos=C3=A9"
  end

  test "escapes empty segments" do
    assert {:ok, topic} = Topic.new(":")
    assert Topic.tokens(topic) == "=.="
  end

  test "escapes a literal dot inside a segment" do
    assert {:ok, topic} = Topic.new("a.b:c")
    assert Topic.tokens(topic) == "a=2Eb.c"
  end

  test "rejects the empty topic" do
    assert Topic.new("") == {:error, :empty}
  end

  test "round trips through tokens" do
    assert {:ok, topic} = Topic.new("room:lobby")
    assert {:ok, decoded} = Topic.from_tokens(Topic.tokens(topic))
    assert decoded == topic
  end
end
