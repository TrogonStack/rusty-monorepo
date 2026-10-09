defmodule TrogonPresence.Tracker do
  @moduledoc """
  The runtime behind a `use TrogonPresence` module: one process per presence
  module, registered under that module's own name, that

    * starts a `TrogonPresence.Reader` the first time a topic is tracked,
      listed or looked up, and keeps its installed snapshot in process state
      (the "ETS replica" worklist item lives here as plain process state
      rather than a separate ETS table, since this process is already the
      only reader and writer of it; see the module-level trade-off note
      below);
    * monitors every pid that calls `track/4` and releases its entries with
      the service when that pid goes down, the same way `Phoenix.Tracker`
      releases on a channel crash;
    * re-heartbeats every locally tracked entry on a fixed interval, grouped
      by `TrogonPresence.Key` the way `Writer.heartbeat/4` expects;
    * rebroadcasts every reader diff as a `"presence_diff"` via
      `Phoenix.PubSub`, shaped by `TrogonPresence.Broadcast`.

  ## Trade-offs against `Phoenix.Presence`

  Phoenix's own `Tracker` keeps a `Task.Supervisor` so a slow `fetch/2`
  callback cannot block the next diff; this module calls `fetch/2` inline on
  the calling process (for `track/list/get_by_key`) or on the `Tracker`
  process itself (for a `presence_diff` broadcast). A slow `fetch/2` here
  therefore delays the next event this process handles. This was accepted to
  keep one process doing one easily-testable thing, rather than a
  supervisor/task pair mirroring internals the Rust side has no equivalent
  of.

  A `Writer.heartbeat/4` reply carries one status per submitted entry. On
  `"gone"` or `"conflict"` (the service no longer recognizes that holder,
  typically after a GC pause or a partition outlasted the lease), this
  process re-tracks the entry under a fresh `HolderId` rather than letting it
  disappear for good; `"unavailable"` is left for the next tick's natural
  retry. A re-track changes the entry's `phx_ref`, so a caller still holding
  the `phx_ref` returned by its original `track/4` or `update/4` call will
  find that reference stale after a lease loss.
  """

  use GenServer
  require Logger

  alias TrogonPresence.Tracker.Tracked
  alias TrogonPresence.Snapshot.Limits
  alias TrogonPresence.Wire.HeartbeatReply

  alias TrogonPresence.{
    Broadcast,
    HolderId,
    Key,
    Meta,
    Presences,
    Reader,
    ShardCount,
    StoredRef,
    Topic,
    Writer
  }

  @default_heartbeat_ms :timer.seconds(10)

  @enforce_keys [
    :module,
    :conn,
    :pubsub_server,
    :dispatcher,
    :connection,
    :reader_key,
    :shards,
    :limits,
    :heartbeat_ms
  ]
  defstruct [
    :module,
    :conn,
    :pubsub_server,
    :dispatcher,
    :connection,
    :reader_key,
    :shards,
    :limits,
    :heartbeat_ms,
    :resnapshot_ms,
    readers: %{},
    reader_pids: %{},
    presences: %{},
    tracked: %{},
    tracked_monitors: %{}
  ]

  @spec start_link(module(), keyword()) :: GenServer.on_start()
  def start_link(module, opts) do
    GenServer.start_link(__MODULE__, {module, opts}, name: module)
  end

  @spec track(module(), pid(), String.t(), String.t(), map()) ::
          {:ok, binary()} | {:error, term()}
  def track(module, pid, topic, key, meta),
    do: GenServer.call(module, {:track, pid, topic, key, meta})

  @spec untrack(module(), pid(), String.t(), String.t()) :: :ok
  def untrack(module, pid, topic, key), do: GenServer.call(module, {:untrack, pid, topic, key})

  @spec update(module(), pid(), String.t(), String.t(), map() | (map() -> map())) ::
          {:ok, binary()} | {:error, term()}
  def update(module, pid, topic, key, meta_or_fun),
    do: GenServer.call(module, {:update, pid, topic, key, meta_or_fun})

  @spec list(module(), String.t(), (String.t(), map() -> map())) :: map()
  def list(module, topic, fetch), do: GenServer.call(module, {:list, topic, fetch})

  @spec get_by_key(module(), String.t(), String.t(), (String.t(), map() -> map())) :: term()
  def get_by_key(module, topic, key, fetch),
    do: GenServer.call(module, {:get_by_key, topic, key, fetch})

  @impl GenServer
  def init({module, opts}) do
    conn = Keyword.fetch!(opts, :conn)
    pubsub_server = Keyword.fetch!(opts, :pubsub_server)
    dispatcher = Keyword.get(opts, :dispatcher, Phoenix.Channel.Server)
    shards = Keyword.get(opts, :shards, ShardCount.default())
    limits = Keyword.get(opts, :limits, Limits.default())
    heartbeat_ms = Keyword.get(opts, :heartbeat_ms, @default_heartbeat_ms)
    resnapshot_ms = Keyword.get(opts, :resnapshot_ms)

    connection = TrogonPresence.ConnectionId.generate()
    reader_key = Key.new!("reader:#{connection}")

    Process.send_after(self(), :heartbeat_tick, heartbeat_ms)

    {:ok,
     %__MODULE__{
       module: module,
       conn: conn,
       pubsub_server: pubsub_server,
       dispatcher: dispatcher,
       connection: connection,
       reader_key: reader_key,
       shards: shards,
       limits: limits,
       heartbeat_ms: heartbeat_ms,
       resnapshot_ms: resnapshot_ms
     }}
  end

  @impl GenServer
  def handle_call({:track, pid, topic_raw, key_raw, meta_map}, _from, state) do
    tracked_key = {pid, topic_raw, to_string(key_raw)}

    if Map.has_key?(state.tracked, tracked_key) do
      {:reply, {:error, {:already_tracked, pid, topic_raw, to_string(key_raw)}}, state}
    else
      do_track(state, tracked_key, pid, topic_raw, key_raw, meta_map)
    end
  end

  def handle_call({:untrack, pid, topic_raw, key_raw}, _from, state) do
    tracked_key = {pid, topic_raw, to_string(key_raw)}

    case Map.fetch(state.tracked, tracked_key) do
      {:ok, tracked} ->
        Writer.untrack(
          state.conn,
          tracked.key,
          state.connection,
          tracked.topic,
          tracked.holder,
          tracked.lifetime,
          tracked.mutation_seq
        )

        {:reply, :ok, drop_tracked(state, tracked_key)}

      :error ->
        {:reply, :ok, state}
    end
  end

  def handle_call({:update, pid, topic_raw, key_raw, meta_or_fun}, _from, state) do
    tracked_key = {pid, topic_raw, to_string(key_raw)}

    case Map.fetch(state.tracked, tracked_key) do
      {:ok, tracked} -> handle_update(state, tracked_key, tracked, meta_or_fun)
      :error -> {:reply, {:error, :nopresence}, state}
    end
  end

  def handle_call({:list, topic_raw, fetch}, _from, state) do
    case Topic.new(topic_raw) do
      {:ok, topic} ->
        state = ensure_reader(state, topic)
        grouped = state.presences |> Map.get(topic_raw, %{}) |> Broadcast.presences()
        {:reply, fetch.(topic_raw, grouped), state}

      {:error, _reason} ->
        {:reply, %{}, state}
    end
  end

  def handle_call({:get_by_key, topic_raw, key_raw, fetch}, _from, state) do
    case Topic.new(topic_raw) do
      {:ok, topic} ->
        state = ensure_reader(state, topic)
        string_key = to_string(key_raw)
        grouped = state.presences |> Map.get(topic_raw, %{}) |> Broadcast.presences()

        case Map.fetch(grouped, string_key) do
          {:ok, %{metas: metas}} ->
            %{^string_key => fetched} = fetch.(topic_raw, %{string_key => %{metas: metas}})
            {:reply, fetched, state}

          :error ->
            {:reply, [], state}
        end

      {:error, _reason} ->
        {:reply, [], state}
    end
  end

  defp do_track(state, tracked_key, pid, topic_raw, key_raw, meta_map) do
    with {:ok, topic} <- Topic.new(topic_raw),
         {:ok, key} <- Key.new(to_string(key_raw)),
         {:ok, meta} <- Meta.new(meta_map) do
      holder = HolderId.generate()
      state = ensure_reader(state, topic)

      case Writer.track(state.conn, key, state.connection, topic, holder, meta) do
        {:ok, written} ->
          tracked = %Tracked{
            pid: pid,
            topic: topic,
            key: key,
            holder: holder,
            lifetime: written.lifetime,
            mutation_seq: written.mutation_seq,
            meta: meta,
            phx_ref: written.phx_ref
          }

          state = put_tracked(state, tracked_key, tracked)
          {:reply, {:ok, StoredRef.to_string(written.phx_ref)}, state}

        {:error, reason} ->
          {:reply, {:error, reason}, state}
      end
    else
      {:error, reason} -> {:reply, {:error, reason}, state}
    end
  end

  @impl GenServer
  def handle_info({:trogon_presence_reader, reader_pid, {:snapshot, _cursor, presences}}, state) do
    case Map.fetch(state.reader_pids, reader_pid) do
      {:ok, topic_raw} -> {:noreply, put_presences(state, topic_raw, presences)}
      :error -> {:noreply, state}
    end
  end

  def handle_info({:trogon_presence_reader, reader_pid, {:diff, _cursor, diff}}, state) do
    case Map.fetch(state.reader_pids, reader_pid) do
      {:ok, topic_raw} ->
        current = Map.get(state.presences, topic_raw, %{})
        updated = Presences.apply_diff(current, diff)
        state = put_presences(state, topic_raw, updated)
        broadcast_diff(state, topic_raw, diff)
        {:noreply, state}

      :error ->
        {:noreply, state}
    end
  end

  def handle_info({:DOWN, _ref, :process, pid, _reason}, state) do
    cond do
      Map.has_key?(state.reader_pids, pid) -> {:noreply, drop_reader(state, pid)}
      Map.has_key?(state.tracked_monitors, pid) -> {:noreply, release_tracked(state, pid)}
      true -> {:noreply, state}
    end
  end

  def handle_info(:heartbeat_tick, state) do
    Process.send_after(self(), :heartbeat_tick, state.heartbeat_ms)
    {:noreply, send_heartbeats(state)}
  end

  def handle_info(_message, state), do: {:noreply, state}

  defp handle_update(state, tracked_key, tracked, meta_or_fun) do
    with {:ok, new_meta} <- resolve_meta(meta_or_fun, tracked.meta) do
      case Writer.update(
             state.conn,
             tracked.key,
             state.connection,
             tracked.topic,
             tracked.holder,
             new_meta,
             tracked.lifetime,
             tracked.mutation_seq
           ) do
        {:ok, written} ->
          updated = %{
            tracked
            | meta: new_meta,
              mutation_seq: written.mutation_seq,
              phx_ref: written.phx_ref
          }

          state = put_tracked(state, tracked_key, updated)
          {:reply, {:ok, StoredRef.to_string(written.phx_ref)}, state}

        {:error, reason} ->
          {:reply, {:error, reason}, state}
      end
    else
      {:error, reason} -> {:reply, {:error, reason}, state}
    end
  end

  defp resolve_meta(fun, current_meta) when is_function(fun, 1),
    do: Meta.new(fun.(Meta.to_map(current_meta)))

  defp resolve_meta(map, _current_meta) when is_map(map), do: Meta.new(map)

  defp put_tracked(state, tracked_key, %Tracked{} = tracked) do
    state
    |> monitor_holder(tracked.pid)
    |> Map.put(:tracked, Map.put(state.tracked, tracked_key, tracked))
  end

  defp drop_tracked(state, tracked_key) do
    %{state | tracked: Map.delete(state.tracked, tracked_key)}
  end

  defp monitor_holder(state, pid) do
    if Map.has_key?(state.tracked_monitors, pid) do
      state
    else
      ref = Process.monitor(pid)
      %{state | tracked_monitors: Map.put(state.tracked_monitors, pid, ref)}
    end
  end

  defp release_tracked(state, pid) do
    {owned, remaining} =
      Enum.split_with(state.tracked, fn {{entry_pid, _topic, _key}, _tracked} ->
        entry_pid == pid
      end)

    Enum.each(owned, fn {_tracked_key, tracked} ->
      Writer.untrack(
        state.conn,
        tracked.key,
        state.connection,
        tracked.topic,
        tracked.holder,
        tracked.lifetime,
        tracked.mutation_seq
      )
    end)

    %{
      state
      | tracked: Map.new(remaining),
        tracked_monitors: Map.delete(state.tracked_monitors, pid)
    }
  end

  defp ensure_reader(state, %Topic{} = topic) do
    topic_raw = Topic.raw(topic)

    if Map.has_key?(state.readers, topic_raw) do
      state
    else
      start_reader(state, topic, topic_raw)
    end
  end

  defp start_reader(state, topic, topic_raw) do
    reader_opts =
      [
        conn: state.conn,
        key: state.reader_key,
        connection: state.connection,
        topic: topic,
        shards: state.shards,
        limits: state.limits,
        subscribers: [self()]
      ]
      |> maybe_put_resnapshot_ms(state.resnapshot_ms)

    case Reader.start_link(reader_opts) do
      {:ok, reader_pid} ->
        Process.monitor(reader_pid)

        %{
          state
          | readers: Map.put(state.readers, topic_raw, reader_pid),
            reader_pids: Map.put(state.reader_pids, reader_pid, topic_raw)
        }

      {:error, reason} ->
        Logger.warning(
          "could not start a trogon_presence reader for #{topic_raw}: #{inspect(reason)}"
        )

        state
    end
  end

  defp maybe_put_resnapshot_ms(opts, nil), do: opts
  defp maybe_put_resnapshot_ms(opts, ms), do: Keyword.put(opts, :resnapshot_ms, ms)

  defp drop_reader(state, reader_pid) do
    case Map.fetch(state.reader_pids, reader_pid) do
      {:ok, topic_raw} ->
        %{
          state
          | readers: Map.delete(state.readers, topic_raw),
            reader_pids: Map.delete(state.reader_pids, reader_pid),
            presences: Map.delete(state.presences, topic_raw)
        }

      :error ->
        state
    end
  end

  defp put_presences(state, topic_raw, presences) do
    %{state | presences: Map.put(state.presences, topic_raw, presences)}
  end

  defp broadcast_diff(state, topic_raw, diff) do
    payload = Broadcast.diff_payload(diff)

    Phoenix.PubSub.local_broadcast(
      state.pubsub_server,
      topic_raw,
      Broadcast.broadcast(topic_raw, payload),
      state.dispatcher
    )
  end

  defp send_heartbeats(state) do
    state.tracked
    |> Map.values()
    |> Enum.group_by(& &1.key)
    |> Enum.reduce(state, fn {key, entries}, state ->
      wire_entries = Enum.map(entries, &{&1.holder, &1.topic, &1.lifetime, &1.mutation_seq})

      case Writer.heartbeat(state.conn, key, state.connection, wire_entries) do
        {:ok, %HeartbeatReply{entries: statuses}} ->
          entries
          |> Enum.zip(statuses)
          |> Enum.reduce(state, &apply_heartbeat_status(&2, &1))

        {:error, reason} ->
          Logger.warning("heartbeat request failed for #{inspect(key)}: #{inspect(reason)}")
          state
      end
    end)
  end

  defp apply_heartbeat_status(state, {%Tracked{} = tracked, status})
       when status in ["gone", "conflict"],
       do: retrack(state, tracked)

  defp apply_heartbeat_status(state, {%Tracked{}, _status}), do: state

  defp retrack(state, %Tracked{} = tracked) do
    holder = HolderId.generate()

    case Writer.track(
           state.conn,
           tracked.key,
           state.connection,
           tracked.topic,
           holder,
           tracked.meta
         ) do
      {:ok, written} ->
        updated = %{
          tracked
          | holder: holder,
            lifetime: written.lifetime,
            mutation_seq: written.mutation_seq,
            phx_ref: written.phx_ref
        }

        tracked_key = {tracked.pid, Topic.raw(tracked.topic), Key.raw(tracked.key)}
        %{state | tracked: Map.put(state.tracked, tracked_key, updated)}

      {:error, reason} ->
        Logger.warning(
          "could not re-track a holder whose lease was lost for #{inspect(tracked.key)}: #{inspect(reason)}"
        )

        state
    end
  end
end
