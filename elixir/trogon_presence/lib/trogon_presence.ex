defmodule TrogonPresence do
  @moduledoc """
  A `Phoenix.Presence` replacement backed by the NATS-based
  `trogon_presence`/`trogon_presence_service` system instead of Phoenix's own
  in-memory, Erlang-distribution-based `Phoenix.Tracker`.

  Use it exactly like `Phoenix.Presence`:

      defmodule MyAppWeb.Presence do
        use TrogonPresence,
          conn: MyApp.Gnat,
          pubsub_server: MyApp.PubSub
      end

  `:conn` names the `Gnat` connection to use and `:pubsub_server` the
  `Phoenix.PubSub` to broadcast `"presence_diff"` events on. `:pubsub_server`
  is required at `use` time, matching `Phoenix.Presence`'s own requirement;
  `:conn` can be given then too, or deferred and supplied later through
  `child_spec/1`'s own `opts` (for example when a test starts a fresh `Gnat`
  connection per module and only then knows which one to track through).
  This module has no `:otp_app`/application-config merge, since nothing
  about this shim needs one: every option it reads is either given to `use`
  or to the child spec that starts it.

  ## `:backend`

  A migration from `Phoenix.Presence` to this shim does not flip in one
  step. `:backend` picks the stage:

    * `:phoenix` - a pure passthrough to a real `Phoenix.Presence`. Nothing
      talks to NATS. Use this to confirm the host app wires `TrogonPresence`
      in correctly before anything behavioral changes.
    * `:dual` - writes go to Phoenix.Tracker first and are then mirrored,
      best-effort, to NATS. Reads and broadcasts stay on Phoenix, so clients
      see exactly what they saw before. The ref a caller gets back from
      `track/4`/`update/4` is Phoenix's own ref, since that is what `list/1`
      will keep showing until the next stage.
    * `:nats_read` - writes still go to both, but reads, `list/1` diffs and
      the ref returned from `track/4`/`update/4` now come from NATS. This is
      the one point in the migration where an entry's ref changes out from
      under a caller that cached it; `:dual` is the rollback.
    * `:nats` (the default) - Phoenix is dropped. Rolling back from here
      means a full resync, not a flip.

  In every stage a channel gets exactly one `"presence_diff"` per change:
  `:phoenix` and `:dual` broadcast through Phoenix.Tracker's own PubSub
  dispatch (the NATS mirror in `:dual` never starts a reader, so it has
  nothing to broadcast from); `:nats_read` and `:nats` broadcast from the
  NATS-sourced diff only, because the `Phoenix.Presence` module mirroring
  writes in `:nats_read` is wired to a PubSub server nothing subscribes to.

  ### Why a ref is minted, not copied, going into `:nats_read`

  The temptation is to reuse the Phoenix ref as the NATS-side ref too, so a
  caller never sees it change. The underlying `trogon_presence_service`
  does not accept a caller-supplied ref on track or update; it always mints
  one. Forcing the Phoenix ref through as if it were the service's own ref
  would mean two different storage layers claiming authorship of the same
  opaque value, which breaks the service's own conflict and generation
  tracking the first time the entry is heartbeated or re-tracked. The one
  user-visible ref change happens exactly once, at the `:dual` -> `:nats_read`
  boundary, and only for a caller that cached the ref rather than re-reading
  `list/1`.

  ### What happens when the NATS write fails but Phoenix succeeded

  Phoenix is always written first and is authoritative for the call's
  result: if Phoenix accepts a `track/4` or `update/4`, so does the caller,
  regardless of whether the NATS mirror write that follows succeeds. A
  failed mirror write is logged and the entry is marked unmirrored. On
  `:dual` this is invisible to clients, since nothing reads from NATS yet;
  the cost is paid later, at the `:nats_read` boundary, where an unmirrored
  entry is simply absent until the next heartbeat tick repairs it (a
  client-side-only rejection, such as a meta that fails this shim's own
  validation though Phoenix accepted it, can never repair itself and stays
  unmirrored for its whole lifetime). On `:nats_read` the cost is immediate:
  the entry is absent from NATS-sourced reads until that same repair lands.
  `untrack` is not retried on a failed mirror release; the orphaned NATS
  entry is harmless and expires with its own lease.

  ### Meta keys after the NATS round trip

  A meta map tracked through `:nats` or `:nats_read` is JSON-encoded on the
  way into the service and decoded back on the way out, so every key a
  caller did not give as a string comes back as one; a `Phoenix.Presence`
  reading the same map back from its own ETS table would hand back whatever
  key type the caller originally used, atom or string. `phx_ref` and
  `phx_ref_prev` are the one exception, converted back to atoms on the way
  out, because `Phoenix.Presence` itself always keys those two as atoms and
  channel-side code matches on them that way. This shim does not guess at
  the rest: converting arbitrary caller-supplied string keys back to atoms
  would grow the atom table on data it does not control. Callers whose own
  meta uses atom keys need to read them back as strings, or track with
  string keys to begin with and avoid the difference; `:phoenix` and `:dual`
  are unaffected, since their reads never leave Phoenix's own ETS table.

  Everything below mirrors `Phoenix.Presence`'s own contract field for
  field; see `TrogonPresence.Tracker` for the process this delegates to and
  the trade-offs it makes against `Phoenix.Tracker`.
  """

  @type presences :: TrogonPresence.Presences.t()
  @type presence :: %{key: String.t(), meta: map()}
  @type topic :: String.t()
  @type backend :: :phoenix | :dual | :nats_read | :nats

  @callback track(socket :: Phoenix.Socket.t(), key :: String.t(), meta :: map()) ::
              {:ok, ref :: binary()} | {:error, reason :: term()}
  @callback track(pid, topic, key :: String.t(), meta :: map()) ::
              {:ok, ref :: binary()} | {:error, reason :: term()}
  @callback untrack(socket :: Phoenix.Socket.t(), key :: String.t()) :: :ok
  @callback untrack(pid, topic, key :: String.t()) :: :ok
  @callback update(
              socket :: Phoenix.Socket.t(),
              key :: String.t(),
              meta :: map() | (map() -> map())
            ) ::
              {:ok, ref :: binary()} | {:error, reason :: term()}
  @callback update(pid, topic, key :: String.t(), meta :: map() | (map() -> map())) ::
              {:ok, ref :: binary()} | {:error, reason :: term()}
  @callback list(socket_or_topic :: Phoenix.Socket.t() | topic) :: presences()
  @callback get_by_key(Phoenix.Socket.t() | topic, key :: String.t()) :: term()
  @callback fetch(topic, presences()) :: presences()

  @backends [:phoenix, :dual, :nats_read, :nats]

  defmacro __using__(opts) do
    opts[:pubsub_server] || raise "use TrogonPresence expects :pubsub_server to be given"
    caller_module = __CALLER__.module
    backend = Keyword.get(opts, :backend, :nats)

    case backend do
      :phoenix ->
        phoenix_backend(opts)

      :nats ->
        nats_backend(opts, caller_module)

      :dual ->
        dual_backend(opts, caller_module)

      :nats_read ->
        nats_read_backend(opts, caller_module)

      other ->
        raise ArgumentError,
              "use TrogonPresence :backend must be one of #{inspect(@backends)}, got #{inspect(other)}"
    end
  end

  defp phoenix_forward_opts(opts) do
    opts
    |> Keyword.take([:otp_app, :pubsub_server])
    |> Keyword.put_new(:otp_app, :trogon_presence)
  end

  defp phoenix_backend(opts) do
    forwarded = phoenix_forward_opts(opts)

    quote do
      # `Phoenix.Presence`'s own behaviour already covers this stage's whole
      # contract; declaring `@behaviour TrogonPresence` too would conflict
      # with it on every shared callback name instead of adding anything.
      use Phoenix.Presence, unquote(forwarded)
    end
  end

  defp nats_backend(opts, caller_module) do
    task_supervisor = Module.concat(caller_module, TaskSupervisor)

    quote location: :keep,
          bind_quoted: [opts: opts, task_supervisor: task_supervisor],
          unquote: true do
      @behaviour TrogonPresence
      @trogon_presence_opts Keyword.merge(opts, backend: :nats, task_supervisor: task_supervisor)
      @trogon_presence_task_supervisor task_supervisor

      def fetch(_topic, presences), do: presences
      defoverridable fetch: 2

      def child_spec(opts) do
        opts = Keyword.merge(@trogon_presence_opts, opts)

        children = [
          {Task.Supervisor, name: @trogon_presence_task_supervisor},
          %{
            id: TrogonPresence.Tracker,
            start: {TrogonPresence.Tracker, :start_link, [__MODULE__, opts]}
          }
        ]

        %{
          id: __MODULE__,
          start:
            {Supervisor, :start_link,
             [children, [strategy: :rest_for_one, name: Module.concat(__MODULE__, Supervisor)]]}
        }
      end

      unquote(TrogonPresence.__write_api__())
      unquote(TrogonPresence.__nats_read_api__())
    end
  end

  defp dual_backend(opts, caller_module) do
    forwarded = phoenix_forward_opts(opts)
    task_supervisor = Module.concat(caller_module, TaskSupervisor)

    quote location: :keep,
          bind_quoted: [
            opts: opts,
            forwarded: forwarded,
            task_supervisor: task_supervisor
          ],
          unquote: true do
      @behaviour TrogonPresence
      @trogon_presence_opts Keyword.merge(opts, backend: :dual, task_supervisor: task_supervisor)
      @trogon_presence_task_supervisor task_supervisor

      defmodule PhoenixMirror do
        @moduledoc false
        use Phoenix.Presence, forwarded
      end

      def fetch(_topic, presences), do: presences
      defoverridable fetch: 2

      def child_spec(opts) do
        opts =
          @trogon_presence_opts
          |> Keyword.merge(opts)
          |> Keyword.put(:phoenix_mirror, __MODULE__.PhoenixMirror)

        children = [
          {Task.Supervisor, name: @trogon_presence_task_supervisor},
          {__MODULE__.PhoenixMirror, []},
          %{
            id: TrogonPresence.Tracker,
            start: {TrogonPresence.Tracker, :start_link, [__MODULE__, opts]}
          }
        ]

        %{
          id: __MODULE__,
          start:
            {Supervisor, :start_link,
             [children, [strategy: :rest_for_one, name: Module.concat(__MODULE__, Supervisor)]]}
        }
      end

      unquote(TrogonPresence.__write_api__())
      unquote(TrogonPresence.__phoenix_read_api__())
    end
  end

  defp nats_read_backend(opts, caller_module) do
    shadow_pubsub = Module.concat(caller_module, PhoenixMirrorPubSub)

    forwarded =
      opts
      |> phoenix_forward_opts()
      |> Keyword.put(:pubsub_server, shadow_pubsub)

    task_supervisor = Module.concat(caller_module, TaskSupervisor)

    quote location: :keep,
          bind_quoted: [
            opts: opts,
            forwarded: forwarded,
            shadow_pubsub: shadow_pubsub,
            task_supervisor: task_supervisor
          ],
          unquote: true do
      @behaviour TrogonPresence

      @trogon_presence_opts Keyword.merge(opts,
                              backend: :nats_read,
                              task_supervisor: task_supervisor
                            )
      @trogon_presence_task_supervisor task_supervisor
      @trogon_presence_shadow_pubsub shadow_pubsub

      defmodule PhoenixMirror do
        @moduledoc false
        use Phoenix.Presence, forwarded
      end

      def fetch(_topic, presences), do: presences
      defoverridable fetch: 2

      def child_spec(opts) do
        opts =
          @trogon_presence_opts
          |> Keyword.merge(opts)
          |> Keyword.put(:phoenix_mirror, __MODULE__.PhoenixMirror)

        children = [
          {Task.Supervisor, name: @trogon_presence_task_supervisor},
          {Phoenix.PubSub, name: @trogon_presence_shadow_pubsub},
          {__MODULE__.PhoenixMirror, []},
          %{
            id: TrogonPresence.Tracker,
            start: {TrogonPresence.Tracker, :start_link, [__MODULE__, opts]}
          }
        ]

        %{
          id: __MODULE__,
          start:
            {Supervisor, :start_link,
             [children, [strategy: :rest_for_one, name: Module.concat(__MODULE__, Supervisor)]]}
        }
      end

      unquote(TrogonPresence.__write_api__())
      unquote(TrogonPresence.__nats_read_api__())
    end
  end

  # The pieces below are shared verbatim across the `:nats`, `:dual` and
  # `:nats_read` code generators. They live as public functions returning
  # quoted code (rather than inline in each generator) purely so the three
  # generators above do not repeat the same clauses three times; callers
  # outside this module have no reason to call them directly.

  @doc false
  def __write_api__ do
    quote do
      def track(%Phoenix.Socket{} = socket, key, meta),
        do: track(socket.channel_pid, socket.topic, key, meta)

      def track(pid, topic, key, meta),
        do: TrogonPresence.Tracker.track(__MODULE__, pid, topic, to_string(key), meta)

      def untrack(%Phoenix.Socket{} = socket, key),
        do: untrack(socket.channel_pid, socket.topic, key)

      def untrack(pid, topic, key),
        do: TrogonPresence.Tracker.untrack(__MODULE__, pid, topic, to_string(key))

      def update(%Phoenix.Socket{} = socket, key, meta),
        do: update(socket.channel_pid, socket.topic, key, meta)

      def update(pid, topic, key, meta),
        do: TrogonPresence.Tracker.update(__MODULE__, pid, topic, to_string(key), meta)
    end
  end

  @doc false
  def __nats_read_api__ do
    quote do
      def list(%Phoenix.Socket{topic: topic}), do: list(topic)
      def list(topic), do: TrogonPresence.Tracker.list(__MODULE__, topic, &__MODULE__.fetch/2)

      def get_by_key(%Phoenix.Socket{topic: topic}, key), do: get_by_key(topic, key)

      def get_by_key(topic, key),
        do:
          TrogonPresence.Tracker.get_by_key(
            __MODULE__,
            topic,
            to_string(key),
            &__MODULE__.fetch/2
          )
    end
  end

  @doc false
  def __phoenix_read_api__ do
    quote do
      def list(%Phoenix.Socket{topic: topic}), do: list(topic)
      def list(topic), do: __MODULE__.fetch(topic, __MODULE__.PhoenixMirror.list(topic))

      def get_by_key(%Phoenix.Socket{topic: topic}, key), do: get_by_key(topic, key)

      def get_by_key(topic, key) do
        case __MODULE__.PhoenixMirror.get_by_key(topic, key) do
          [] ->
            []

          %{metas: _metas} = entry ->
            string_key = to_string(key)
            %{^string_key => fetched} = __MODULE__.fetch(topic, %{string_key => entry})
            fetched
        end
      end
    end
  end
end
