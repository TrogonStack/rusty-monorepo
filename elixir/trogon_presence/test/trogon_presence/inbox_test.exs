defmodule TrogonPresence.InboxTest do
  use ExUnit.Case, async: true

  alias TrogonPresence.{ConnectionId, Inbox, Key, RequestId}

  test "builds a reply subject the service's scoped validation accepts" do
    {:ok, key} = Key.new("ana@x.io")
    connection = ConnectionId.generate()
    request = RequestId.generate()

    subject = Inbox.reply(key, connection, request)

    assert subject == "_INBOX_U.ana=40x=2Eio.#{connection}.#{request}"
    assert String.starts_with?(subject, "_INBOX_U.ana=40x=2Eio.")

    tail =
      subject
      |> String.trim_leading("_INBOX_U.ana=40x=2Eio.")
      |> String.split(".")

    assert length(tail) == 2
  end
end
