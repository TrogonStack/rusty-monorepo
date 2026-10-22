defmodule TrogonPresence.Test.Presence do
  @moduledoc """
  The presence module a test channel tracks through. `:conn` is a
  placeholder at compile time (`nil`): `child_spec/1` merges whatever is
  passed to `start_supervised!({TrogonPresence.Test.Presence, conn: conn})`
  over it, so each test module's own freshly started `Gnat` connection is
  what actually gets used.
  """

  use TrogonPresence,
    conn: nil,
    pubsub_server: TrogonPresence.Test.PubSub
end
