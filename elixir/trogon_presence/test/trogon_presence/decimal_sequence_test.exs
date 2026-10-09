defmodule TrogonPresence.DecimalSequenceTest do
  use ExUnit.Case, async: true

  alias TrogonPresence.MutationSequence

  @max_u64 0xFFFFFFFFFFFFFFFF

  test "round trips boundary values as decimal strings" do
    for raw <- [0, 1, 42, @max_u64 - 1, @max_u64] do
      text = raw |> MutationSequence.from_integer() |> MutationSequence.to_string()
      assert {:ok, decoded} = MutationSequence.parse(text)
      assert MutationSequence.to_string(decoded) == text
      assert decoded == MutationSequence.from_integer(raw)
    end
  end

  test "rejects non canonical encodings" do
    for bad <- ["01", "", "-1", "+1", "1.0", "18446744073709551616"] do
      assert MutationSequence.parse(bad) == :error, bad
    end
  end

  test "compares by numeric value" do
    low = MutationSequence.from_integer(1)
    high = MutationSequence.from_integer(2)
    assert MutationSequence.compare(low, high) == :lt
    assert MutationSequence.compare(high, low) == :gt
    assert MutationSequence.compare(low, low) == :eq
  end
end
