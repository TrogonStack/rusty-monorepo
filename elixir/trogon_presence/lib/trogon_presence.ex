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

  Everything below mirrors `Phoenix.Presence`'s own contract field for
  field; see `TrogonPresence.Tracker` for the process this delegates to and
  the trade-offs it makes against `Phoenix.Tracker`.
  """

  @type presences :: TrogonPresence.Presences.t()
  @type presence :: %{key: String.t(), meta: map()}
  @type topic :: String.t()

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

  defmacro __using__(opts) do
    quote location: :keep, bind_quoted: [opts: opts] do
      @behaviour TrogonPresence
      @trogon_presence_opts opts

      _ = opts[:pubsub_server] || raise "use TrogonPresence expects :pubsub_server to be given"

      def fetch(_topic, presences), do: presences
      defoverridable fetch: 2

      def child_spec(opts) do
        opts = Keyword.merge(@trogon_presence_opts, opts)

        %{
          id: __MODULE__,
          start: {TrogonPresence.Tracker, :start_link, [__MODULE__, opts]}
        }
      end

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
end
