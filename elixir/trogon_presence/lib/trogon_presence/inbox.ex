defmodule TrogonPresence.Inbox do
  @moduledoc """
  Builds a reply-to subject under the service's `_INBOX_U` caller namespace
  (`inbox.rs`). The service validates a write, holder or snapshot request's
  reply subject structurally against `_INBOX_U.<key_token>.<connection>.<...>`,
  independent of NATS ACLs, so the shim cannot rely on a generic client's
  default `_INBOX` request/reply mux: every request this shim sends
  publishes with an explicit `reply_to` built here, and subscribes to that
  same subject itself before publishing.
  """

  alias TrogonPresence.{ConnectionId, Key, RequestId}

  @prefix "_INBOX_U"

  @spec reply(Key.t(), ConnectionId.t(), RequestId.t()) :: String.t()
  def reply(key, connection, request) do
    "#{@prefix}.#{Key.token(key)}.#{connection}.#{request}"
  end
end
