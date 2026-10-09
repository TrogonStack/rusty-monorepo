defmodule TrogonPresence.Writer do
  @moduledoc """
  Synchronous track, update, untrack, heartbeat and release calls against the
  presence service, per A21.9's default of a synchronous write before a
  caller treats itself as tracked.

  Every call hand-builds its own `_INBOX_U`-scoped reply subject
  (`TrogonPresence.Inbox`), subscribes to it, publishes the command with
  `reply_to` set to that subject, and waits for the single reply. This
  cannot use `Gnat.request/4`: that helper generates a reply subject under
  the unscoped `_INBOX` mux, which the service's ingress structurally
  refuses (`inbox.rs`), independent of any NATS ACL.
  """

  alias TrogonPresence.{Inbox, RequestId, Subjects}

  alias TrogonPresence.Wire.{
    ErrorReply,
    HeartbeatReply,
    ReleaseReply,
    UntrackedReply,
    WrittenReply
  }

  @default_timeout :timer.seconds(5)

  @type conn :: GenServer.server()
  @type reply(ok) :: {:ok, ok} | {:error, ErrorReply.t() | :timeout | {:transport, term()}}

  @spec track(
          conn(),
          TrogonPresence.Key.t(),
          TrogonPresence.ConnectionId.t(),
          TrogonPresence.Topic.t(),
          TrogonPresence.HolderId.t(),
          TrogonPresence.Meta.t(),
          keyword()
        ) ::
          reply(WrittenReply.t())
  def track(conn, key, connection, topic, holder, meta, opts \\ []) do
    body = %{holder: holder, meta: meta}
    request(conn, key, connection, Subjects.track(key, topic), body, &WrittenReply.decode/1, opts)
  end

  @spec update(
          conn(),
          TrogonPresence.Key.t(),
          TrogonPresence.ConnectionId.t(),
          TrogonPresence.Topic.t(),
          TrogonPresence.HolderId.t(),
          TrogonPresence.Meta.t(),
          TrogonPresence.LifetimeId.t(),
          TrogonPresence.MutationSequence.t(),
          keyword()
        ) :: reply(WrittenReply.t())
  def update(conn, key, connection, topic, holder, meta, lifetime, mutation_seq, opts \\ []) do
    body =
      %{holder: holder, meta: meta, lifetime: lifetime, mutation_seq: mutation_seq}
      |> maybe_put(:expected_ref, Keyword.get(opts, :expected_ref))

    request(
      conn,
      key,
      connection,
      Subjects.update(key, topic),
      body,
      &WrittenReply.decode/1,
      opts
    )
  end

  @spec untrack(
          conn(),
          TrogonPresence.Key.t(),
          TrogonPresence.ConnectionId.t(),
          TrogonPresence.Topic.t(),
          TrogonPresence.HolderId.t(),
          TrogonPresence.LifetimeId.t(),
          TrogonPresence.MutationSequence.t(),
          keyword()
        ) :: reply(UntrackedReply.t())
  def untrack(conn, key, connection, topic, holder, lifetime, mutation_seq, opts \\ []) do
    body = %{holder: holder, lifetime: lifetime, mutation_seq: mutation_seq}

    request(
      conn,
      key,
      connection,
      Subjects.untrack(key, topic),
      body,
      &UntrackedReply.decode/1,
      opts
    )
  end

  @spec heartbeat(
          conn(),
          TrogonPresence.Key.t(),
          TrogonPresence.ConnectionId.t(),
          [
            {TrogonPresence.HolderId.t(), TrogonPresence.Topic.t(), TrogonPresence.LifetimeId.t(),
             TrogonPresence.MutationSequence.t()}
          ],
          keyword()
        ) :: reply(HeartbeatReply.t())
  def heartbeat(conn, key, connection, entries, opts \\ []) do
    body = %{entries: Enum.map(entries, &beat_wire/1)}
    request(conn, key, connection, Subjects.heartbeat(key), body, &HeartbeatReply.decode/1, opts)
  end

  @spec release(
          conn(),
          TrogonPresence.Key.t(),
          TrogonPresence.ConnectionId.t(),
          TrogonPresence.HolderId.t(),
          [
            {TrogonPresence.Topic.t(), TrogonPresence.LifetimeId.t()}
          ],
          keyword()
        ) :: reply(ReleaseReply.t())
  def release(conn, key, connection, holder, targets, opts \\ []) do
    body = %{holder: holder, targets: Enum.map(targets, &release_wire/1)}
    request(conn, key, connection, Subjects.release(key), body, &ReleaseReply.decode/1, opts)
  end

  defp beat_wire({holder, topic, lifetime, mutation_seq}) do
    %{holder: holder, topic: topic, lifetime: lifetime, mutation_seq: mutation_seq}
  end

  defp release_wire({topic, lifetime}), do: %{topic: topic, lifetime: lifetime}

  defp maybe_put(map, _key, nil), do: map
  defp maybe_put(map, key, value), do: Map.put(map, key, value)

  defp request(conn, key, connection, subject, body, decode_ok, opts) do
    timeout = Keyword.get(opts, :timeout, @default_timeout)
    inbox = Inbox.reply(key, connection, RequestId.generate())

    case Jason.encode(body) do
      {:ok, payload} -> send_and_await(conn, subject, payload, inbox, decode_ok, timeout)
      {:error, reason} -> {:error, {:transport, {:encode_failed, reason}}}
    end
  end

  defp send_and_await(conn, subject, payload, inbox, decode_ok, timeout) do
    case Gnat.sub(conn, self(), inbox) do
      {:ok, sid} ->
        result =
          case Gnat.pub(conn, subject, payload, reply_to: inbox) do
            :ok -> await_reply(sid, decode_ok, timeout)
            {:error, reason} -> {:error, {:transport, reason}}
          end

        Gnat.unsub(conn, sid)
        result

      {:error, reason} ->
        {:error, {:transport, reason}}
    end
  end

  defp await_reply(sid, decode_ok, timeout) do
    receive do
      {:msg, %{sid: ^sid, body: body}} -> decode_response(body, decode_ok)
    after
      timeout -> {:error, :timeout}
    end
  end

  defp decode_response(body, decode_ok) do
    case Jason.decode(body) do
      {:ok, %{"error" => _} = map} -> {:error, ErrorReply.decode(map)}
      {:ok, map} -> {:ok, decode_ok.(map)}
      {:error, reason} -> {:error, {:transport, {:invalid_json, reason}}}
    end
  end
end
