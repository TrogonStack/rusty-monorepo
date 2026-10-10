defmodule TrogonPresence.Reader.Installed do
  @moduledoc false

  alias TrogonPresence.{Presences, ViewCursor}

  @enforce_keys [:cursor, :state]
  defstruct [:cursor, :state]
  @type t :: %__MODULE__{cursor: ViewCursor.t(), state: Presences.t()}
end

defmodule TrogonPresence.Reader.BufferedFrame do
  @moduledoc false

  alias TrogonPresence.{Diff, DiffSequence, ViewCursor}

  @enforce_keys [:cursor, :prev, :diff]
  defstruct [:cursor, :prev, :diff]
  @type t :: %__MODULE__{cursor: ViewCursor.t(), prev: DiffSequence.t(), diff: Diff.t() | nil}
end

defmodule TrogonPresence.Reader.Link do
  @moduledoc false

  @enforce_keys [:pid, :monitor, :diff, :parts, :replies, :epoch]
  defstruct [:pid, :monitor, :diff, :parts, :replies, :epoch]

  @type sid :: non_neg_integer() | String.t()
  @type t :: %__MODULE__{
          pid: pid(),
          monitor: reference(),
          diff: sid(),
          parts: sid(),
          replies: sid(),
          epoch: sid()
        }

  @spec sids(t()) :: [sid()]
  def sids(%__MODULE__{} = link), do: [link.diff, link.parts, link.replies, link.epoch]
end

defmodule TrogonPresence.Reader.Pending do
  @moduledoc false

  alias TrogonPresence.RequestId
  alias TrogonPresence.Reader.BufferedFrame
  alias TrogonPresence.Snapshot.{Assembly, Frame}

  @enforce_keys [
    :request,
    :deadline_timer,
    :assembly,
    :stash,
    :stash_count,
    :stash_bytes,
    :buffer,
    :buffer_bytes
  ]
  defstruct [
    :request,
    :deadline_timer,
    :assembly,
    :stash,
    :stash_count,
    :stash_bytes,
    :buffer,
    :buffer_bytes
  ]

  @type t :: %__MODULE__{
          request: RequestId.t(),
          deadline_timer: reference(),
          assembly: Assembly.t() | nil,
          stash: [Frame.t()],
          stash_count: non_neg_integer(),
          stash_bytes: non_neg_integer(),
          buffer: :queue.queue(BufferedFrame.t()),
          buffer_bytes: non_neg_integer()
        }
end

defmodule TrogonPresence.Reader do
  @moduledoc """
  Follows a topic's diff log the same way
  `trogon_presence_service::reader::Machine` does: request a snapshot, stash
  any diff/snapshot-part frames that arrive before it is granted, assemble
  the snapshot's parts once a manifest arrives, install it, then apply each
  buffered and subsequently-delivered diff frame in cursor order, retrying
  with jittered backoff and resnapshotting on a gap, a rebase, a stale epoch
  hint, or a periodic timer.

  Deliberately scoped to the diff/snapshot state machine only. It keeps no
  ETS table and makes no `Phoenix.PubSub` broadcast itself; a subscriber
  (passed via the `:subscribers` start option, or added with `subscribe/2`)
  receives `{:trogon_presence_reader, pid, {:snapshot, cursor, presences}}`
  and `{:trogon_presence_reader, pid, {:diff, cursor, diff}}` messages, and
  decides what to do with them (write an ETS replica, rebroadcast as a
  `presence_diff`, or just assert on them in a test). `presences/1` reads the
  currently installed state directly off the process, standing in for the
  Rust reader's `watch::Receiver<Presences>`.

  ## Reconnects

  The Rust reader resnapshots when its client's reconnect count changes.
  `gnat` keeps no such count and never reconnects in place: a `Gnat`
  process dies with its socket, taking every subscription with it, and
  `Gnat.ConnectionSupervisor` starts a fresh process under the same
  registered name. So this process monitors the connection process it
  subscribed on. When that process goes down and `:conn` is a registered
  name, it keeps serving the installed (now possibly stale) state, polls the
  name every 50ms until a new connection is registered, subscribes again
  on it, and requests a fresh snapshot straight away instead of waiting for
  the periodic timer. Frames still in the mailbox from the old connection
  are ignored. When `:conn` is a bare pid there is nothing to reconnect to,
  so this process stops with `{:shutdown, {:connection_down, reason}}`.
  """

  use GenServer
  require Logger

  alias TrogonPresence.Reader.{BufferedFrame, Installed, Link, Pending}
  alias TrogonPresence.Snapshot.{Assembly, Frame, Identity, Limits, Manifest}

  alias TrogonPresence.{
    Diff,
    DiffSequence,
    EntryRevision,
    GenerationEpoch,
    Key,
    LocalViewId,
    OwnerEpoch,
    OwnerId,
    Presences,
    RequestId,
    RetryBackoff,
    ShardCount,
    StreamGeneration,
    Subjects,
    ViewCursor,
    ViewShard
  }

  @inbox_prefix "_INBOX_U"
  @default_resnapshot_ms :timer.seconds(30)
  @reconnect_poll_ms 50

  @header_code "presence-code"
  @header_kind "presence-kind"
  @header_generation "presence-generation"
  @header_owner_id "presence-owner-id"
  @header_owner_rev "presence-owner-rev"
  @header_seq "presence-seq"
  @header_prev "presence-prev"
  @code_ok "ok"
  @kind_diff "diff"
  @kind_keepalive "keepalive"

  @enforce_keys [
    :conn,
    :shards,
    :key,
    :connection,
    :topic,
    :limits,
    :resnapshot_ms,
    :request_subject,
    :inbox_prefix,
    :subscribers
  ]
  defstruct [
    :conn,
    :shards,
    :key,
    :connection,
    :topic,
    :limits,
    :resnapshot_ms,
    :request_subject,
    :inbox_prefix,
    link: nil,
    installed: nil,
    pending: nil,
    retry_timer: nil,
    backoff: nil,
    resnapshot_timer: nil,
    subscribers: []
  ]

  @spec start_link(keyword()) :: GenServer.on_start()
  def start_link(opts) do
    {gen_opts, opts} = Keyword.split(opts, [:name])
    GenServer.start_link(__MODULE__, Map.new(opts), gen_opts)
  end

  @spec presences(GenServer.server()) :: Presences.t()
  def presences(reader), do: GenServer.call(reader, :presences)

  @spec subscribe(GenServer.server(), pid()) :: :ok
  def subscribe(reader, pid \\ self()), do: GenServer.call(reader, {:subscribe, pid})

  @impl GenServer
  def init(args) do
    conn = Map.fetch!(args, :conn)
    shards = Map.get(args, :shards, ShardCount.default())
    key = Map.fetch!(args, :key)
    connection = Map.fetch!(args, :connection)
    topic = Map.fetch!(args, :topic)
    limits = Map.get(args, :limits, Limits.default())
    resnapshot_ms = Map.get(args, :resnapshot_ms, @default_resnapshot_ms)
    subscribers = Map.get(args, :subscribers, [])

    view = LocalViewId.generate()
    inbox_prefix = "#{@inbox_prefix}.#{Key.token(key)}.#{connection}.#{view}"

    state = %__MODULE__{
      conn: conn,
      shards: shards,
      key: key,
      connection: connection,
      topic: topic,
      limits: limits,
      resnapshot_ms: resnapshot_ms,
      request_subject: Subjects.snapshot_request(shards, key, connection, topic),
      inbox_prefix: inbox_prefix,
      backoff: RetryBackoff.default(),
      subscribers: subscribers
    }

    case attach(state) do
      {:ok, state} -> {:ok, request(state)}
      {:error, reason} when is_pid(conn) -> {:stop, reason}
      {:error, _reason} -> {:ok, schedule_reconnect(state)}
    end
  end

  @impl GenServer
  def handle_call(:presences, _from, state) do
    current = if state.installed, do: state.installed.state, else: %{}
    {:reply, current, state}
  end

  def handle_call({:subscribe, pid}, _from, state) do
    {:reply, :ok, %{state | subscribers: [pid | state.subscribers]}}
  end

  @impl GenServer
  def handle_info({:msg, %{gnat: pid, sid: sid} = message}, %{link: %Link{pid: pid}} = state) do
    {:noreply, on_message(state, state.link, sid, message)}
  end

  def handle_info({:DOWN, ref, :process, _pid, reason}, %{link: %Link{monitor: ref}} = state) do
    if is_pid(state.conn) do
      {:stop, {:shutdown, {:connection_down, reason}}, %{state | link: nil}}
    else
      {:noreply, state |> detach() |> schedule_reconnect()}
    end
  end

  def handle_info(:reconnect, %{link: nil} = state) do
    case attach(state) do
      {:ok, state} -> {:noreply, request(state)}
      {:error, _reason} -> {:noreply, schedule_reconnect(state)}
    end
  end

  def handle_info(:retry, %{retry_timer: nil} = state), do: {:noreply, state}

  def handle_info(:retry, state) do
    {:noreply, request(%{state | retry_timer: nil})}
  end

  def handle_info({:snapshot_deadline, request_id}, state) do
    case state.pending do
      %Pending{request: ^request_id} -> {:noreply, abandon(state, :deadline)}
      _other -> {:noreply, state}
    end
  end

  def handle_info(:resnapshot_tick, state) do
    {:noreply, resnapshot(%{state | resnapshot_timer: nil})}
  end

  def handle_info(_message, state), do: {:noreply, state}

  @impl GenServer
  def terminate(_reason, %{link: %Link{} = link}) do
    Enum.each(Link.sids(link), &safe_unsub(link.pid, &1))
  end

  def terminate(_reason, _state), do: :ok

  defp on_message(state, %Link{diff: sid}, sid, message), do: on_diff(state, message)
  defp on_message(state, %Link{parts: sid}, sid, message), do: on_part(state, message)
  defp on_message(state, %Link{replies: sid}, sid, message), do: on_reply(state, message)
  defp on_message(state, %Link{epoch: sid}, sid, message), do: on_epoch(state, message)
  defp on_message(state, %Link{}, _sid, _message), do: state

  defp attach(state) do
    case GenServer.whereis(state.conn) do
      pid when is_pid(pid) -> attach_to(state, pid)
      _other -> {:error, :noproc}
    end
  end

  defp attach_to(state, pid) do
    monitor = Process.monitor(pid)

    subjects = [
      Subjects.diff(state.topic),
      Subjects.snapshot_reply_filter(state.key, state.connection),
      "#{state.inbox_prefix}.*",
      Subjects.epoch(state.shards, ViewShard.of(state.topic, state.shards))
    ]

    case subscribe_all(pid, subjects) do
      {:ok, [diff, parts, replies, epoch]} ->
        link = %Link{
          pid: pid,
          monitor: monitor,
          diff: diff,
          parts: parts,
          replies: replies,
          epoch: epoch
        }

        {:ok, %{state | link: link}}

      {:error, reason} ->
        Process.demonitor(monitor, [:flush])
        {:error, reason}
    end
  end

  defp subscribe_all(pid, subjects) do
    Enum.reduce_while(subjects, {:ok, []}, fn subject, {:ok, sids} ->
      case safe_sub(pid, subject) do
        {:ok, sid} ->
          {:cont, {:ok, [sid | sids]}}

        {:error, reason} ->
          Enum.each(sids, &safe_unsub(pid, &1))
          {:halt, {:error, reason}}
      end
    end)
    |> case do
      {:ok, sids} -> {:ok, Enum.reverse(sids)}
      {:error, reason} -> {:error, reason}
    end
  end

  defp safe_sub(pid, subject) do
    Gnat.sub(pid, self(), subject)
  catch
    :exit, reason -> {:error, reason}
  end

  defp safe_unsub(pid, sid) do
    Gnat.unsub(pid, sid)
  catch
    :exit, _reason -> :ok
  end

  defp safe_pub(pid, subject, body, opts) do
    Gnat.pub(pid, subject, body, opts)
  catch
    :exit, reason -> {:error, reason}
  end

  defp detach(state) do
    cancel_pending_timer(state.pending)
    cancel_retry_timer(state)
    %{state | link: nil, pending: nil, retry_timer: nil, backoff: RetryBackoff.default()}
  end

  defp schedule_reconnect(state) do
    Process.send_after(self(), :reconnect, @reconnect_poll_ms)
    state
  end

  defp request(%{link: nil} = state), do: state
  defp request(%{pending: pending} = state) when not is_nil(pending), do: state

  defp request(state) do
    request_id = RequestId.generate()

    case Jason.encode(%{request_id: request_id}) do
      {:ok, body} -> send_request(state, request_id, body)
      {:error, _reason} -> schedule_retry(state)
    end
  end

  defp send_request(state, request_id, body) do
    reply_to = "#{state.inbox_prefix}.#{request_id}"

    case safe_pub(state.link.pid, state.request_subject, body, reply_to: reply_to) do
      :ok ->
        deadline_timer =
          Process.send_after(self(), {:snapshot_deadline, request_id}, state.limits.deadline_ms)

        pending = %Pending{
          request: request_id,
          deadline_timer: deadline_timer,
          assembly: nil,
          stash: [],
          stash_count: 0,
          stash_bytes: 0,
          buffer: :queue.new(),
          buffer_bytes: 0
        }

        %{state | pending: pending, retry_timer: nil}

      {:error, _reason} ->
        schedule_retry(state)
    end
  end

  defp schedule_retry(state) do
    timer = Process.send_after(self(), :retry, RetryBackoff.jittered_ms(state.backoff))
    %{state | retry_timer: timer, backoff: RetryBackoff.next(state.backoff)}
  end

  defp abandon(state, reason) do
    Logger.debug("abandoning a snapshot assembly: #{inspect(reason)}")
    cancel_pending_timer(state.pending)
    schedule_retry(%{state | pending: nil})
  end

  defp cancel_pending_timer(nil), do: :ok

  defp cancel_pending_timer(%Pending{deadline_timer: timer}),
    do: ignore_timer_result(Process.cancel_timer(timer))

  defp cancel_retry_timer(%{retry_timer: nil}), do: :ok

  defp cancel_retry_timer(%{retry_timer: timer}),
    do: ignore_timer_result(Process.cancel_timer(timer))

  defp cancel_resnapshot_timer(%{resnapshot_timer: nil}), do: :ok

  defp cancel_resnapshot_timer(%{resnapshot_timer: timer}),
    do: ignore_timer_result(Process.cancel_timer(timer))

  defp ignore_timer_result(_result), do: :ok

  defp resnapshot(%{pending: nil, retry_timer: nil} = state), do: request(state)
  defp resnapshot(state), do: state

  defp on_reply(%{pending: nil} = state, _message), do: state

  defp on_reply(%{pending: pending} = state, message) do
    expected = pending.request
    token = message.topic |> String.split(".") |> List.last()

    case RequestId.parse(token) do
      {:ok, ^expected} -> handle_manifest_reply(state, pending, message)
      _other -> state
    end
  end

  defp handle_manifest_reply(state, pending, message) do
    headers = Map.get(message, :headers) || []

    case header(headers, @header_code) do
      {:ok, @code_ok} ->
        case Jason.decode(message.body) do
          {:ok, wire} -> install_manifest(state, pending, wire)
          {:error, _reason} -> abandon(state, {:refused, :invalid_json})
        end

      _other ->
        abandon(state, {:refused, message.body})
    end
  end

  defp install_manifest(state, pending, wire) do
    with {:ok, manifest} <- decode_manifest(wire),
         true <- manifest.identity.request == pending.request,
         true <- is_nil(pending.assembly),
         {:ok, assembly} <- Assembly.begin(manifest, state.limits) do
      ordered_stash = Enum.reverse(pending.stash)
      pending = %{pending | assembly: assembly, stash: [], stash_count: 0, stash_bytes: 0}
      feed_stash(%{state | pending: pending}, ordered_stash)
    else
      {:error, reason} -> abandon(state, {:assembly, reason})
      false -> abandon(state, {:refused, :manifest_mismatch})
      :error -> abandon(state, {:refused, :invalid_manifest})
    end
  end

  defp decode_manifest(wire), do: Manifest.decode(wire)

  defp feed_stash(state, frames) do
    Enum.reduce_while(frames, state, fn frame, state ->
      case feed(state, frame) do
        {state, true} -> {:halt, state}
        {state, false} -> {:cont, state}
      end
    end)
  end

  defp on_part(%{pending: nil} = state, _message), do: state

  defp on_part(state, message) do
    case Frame.decode(message) do
      {:ok, frame} -> accept_part(state, frame)
      :error -> state
    end
  end

  defp accept_part(%{pending: pending} = state, frame) do
    if Frame.identity(frame).request != pending.request do
      state
    else
      accept_part_for_request(state, pending, frame)
    end
  end

  defp accept_part_for_request(state, %Pending{assembly: nil} = pending, frame) do
    stash_part(state, pending, frame)
  end

  defp accept_part_for_request(state, %Pending{}, frame) do
    {state, _done?} = feed(state, frame)
    state
  end

  defp stash_part(state, pending, frame) do
    bytes = frame_payload(frame)
    stash_bytes = pending.stash_bytes + byte_size(bytes)

    if stash_bytes > state.limits.max_bytes or pending.stash_count > state.limits.max_parts do
      abandon(state, :overflow)
    else
      pending = %{
        pending
        | stash: [frame | pending.stash],
          stash_bytes: stash_bytes,
          stash_count: pending.stash_count + 1
      }

      %{state | pending: pending}
    end
  end

  defp frame_payload({:part, _identity, _index, bytes}), do: bytes
  defp frame_payload({:end, _identity, _parts}), do: ""

  defp feed(%{pending: nil} = state, _frame), do: {state, true}
  defp feed(%{pending: %Pending{assembly: nil}} = state, _frame), do: {state, true}

  defp feed(state, frame) do
    assembly = state.pending.assembly
    cursor = Identity.cursor(assembly.manifest.identity)

    case Assembly.accept(assembly, frame) do
      {:ok, assembly, :pending} ->
        {%{state | pending: %{state.pending | assembly: assembly}}, false}

      {:ok, _assembly, {:complete, presences}} ->
        {install(state, cursor, presences), true}

      {:error, reason} ->
        {abandon(state, {:assembly, reason}), true}
    end
  end

  defp install(state, cursor, presences) do
    buffered = if state.pending, do: :queue.to_list(state.pending.buffer), else: []
    cancel_pending_timer(state.pending)
    cancel_resnapshot_timer(state)
    resnapshot_timer = Process.send_after(self(), :resnapshot_tick, state.resnapshot_ms)

    state = %{
      state
      | pending: nil,
        backoff: RetryBackoff.default(),
        resnapshot_timer: resnapshot_timer,
        installed: %Installed{cursor: cursor, state: presences}
    }

    notify(state, {:snapshot, cursor, presences})
    replay_buffered(state, buffered)
  end

  defp replay_buffered(state, buffered) do
    Enum.reduce_while(buffered, state, fn frame, state ->
      case apply_frame(state, frame) do
        {state, true} -> {:cont, state}
        {state, false} -> {:halt, state}
      end
    end)
  end

  defp on_diff(state, message) do
    case parse_diff(message) do
      :error -> resnapshot(state)
      {:ok, frame} -> buffer_or_apply(state, frame, byte_size(message.body))
    end
  end

  defp buffer_or_apply(%{pending: nil} = state, frame, _size) do
    {state, _continue?} = apply_frame(state, frame)
    state
  end

  defp buffer_or_apply(%{pending: pending} = state, frame, size) do
    buffer_bytes = pending.buffer_bytes + size

    if buffer_bytes > state.limits.diff_buffer_bytes do
      abandon(state, :overflow)
    else
      pending = %{pending | buffer: :queue.in(frame, pending.buffer), buffer_bytes: buffer_bytes}
      %{state | pending: pending}
    end
  end

  defp apply_frame(%{installed: nil} = state, _frame), do: {state, false}

  defp apply_frame(state, %BufferedFrame{} = frame) do
    case ViewCursor.follow(state.installed.cursor, frame.cursor, frame.prev) do
      step when step in [:repeat, :stale] -> {state, true}
      step when step in [:gap, :rebase] -> {resnapshot(state), false}
      :next -> {advance(state, frame), true}
    end
  end

  defp advance(state, frame) do
    state = put_in(state.installed.cursor, frame.cursor)

    case frame.diff do
      nil ->
        state

      %Diff{} = diff ->
        new_presences = Presences.apply_diff(state.installed.state, diff)
        state = put_in(state.installed.state, new_presences)
        notify(state, {:diff, frame.cursor, diff})
        state
    end
  end

  defp on_epoch(state, message) do
    case decode_epoch_hint(message.body) do
      {:ok, hinted} -> apply_epoch_hint(state, hinted)
      :error -> resnapshot(state)
    end
  end

  defp apply_epoch_hint(state, hinted) do
    current = current_epoch(state)

    stale? =
      case current && GenerationEpoch.compare(current, hinted) do
        {:ok, :same} -> true
        {:ok, :newer} -> true
        _other -> false
      end

    if stale?, do: state, else: resnapshot(state)
  end

  defp current_epoch(%{installed: nil}), do: nil
  defp current_epoch(%{installed: %Installed{cursor: cursor}}), do: cursor.epoch

  defp decode_epoch_hint(body) do
    with {:ok, wire} <- Jason.decode(body) do
      decode_epoch_hint_wire(wire)
    else
      _other -> :error
    end
  end

  defp decode_epoch_hint_wire(%{
         "generation" => generation,
         "owner_epoch" => %{"acquired" => acquired, "owner" => owner}
       }) do
    with {:ok, generation} <- StreamGeneration.parse(generation),
         {:ok, owner} <- OwnerId.parse(owner),
         {:ok, acquired} <- EntryRevision.parse(acquired) do
      {:ok, GenerationEpoch.new(generation, OwnerEpoch.new(acquired, owner))}
    else
      _other -> :error
    end
  end

  defp decode_epoch_hint_wire(_wire), do: :error

  defp notify(state, event) do
    Enum.each(state.subscribers, &send(&1, {:trogon_presence_reader, self(), event}))
  end

  defp parse_diff(message) do
    headers = Map.get(message, :headers) || []

    with {:ok, generation} <- header_parse(headers, @header_generation, &StreamGeneration.parse/1),
         {:ok, owner} <- header_parse(headers, @header_owner_id, &OwnerId.parse/1),
         {:ok, acquired} <- header_parse(headers, @header_owner_rev, &EntryRevision.parse/1),
         {:ok, seq} <- header_parse(headers, @header_seq, &DiffSequence.parse/1),
         {:ok, prev} <- header_parse(headers, @header_prev, &DiffSequence.parse/1),
         {:ok, kind} <- header(headers, @header_kind),
         {:ok, diff} <- parse_diff_body(kind, message.body) do
      epoch = GenerationEpoch.new(generation, OwnerEpoch.new(acquired, owner))
      cursor = ViewCursor.new(epoch, seq)
      {:ok, %BufferedFrame{cursor: cursor, prev: prev, diff: diff}}
    else
      _other -> :error
    end
  end

  defp parse_diff_body(@kind_diff, body) do
    with {:ok, wire} <- Jason.decode(body),
         {:ok, diff} <- Diff.decode(wire) do
      {:ok, diff}
    end
  end

  defp parse_diff_body(@kind_keepalive, _body), do: {:ok, nil}
  defp parse_diff_body(_kind, _body), do: :error

  defp header(headers, name) do
    case List.keyfind(headers, name, 0) do
      {^name, value} -> {:ok, to_string(value)}
      nil -> :error
    end
  end

  defp header_parse(headers, name, parse) do
    with {:ok, value} <- header(headers, name) do
      parse.(value)
    end
  end
end
