defmodule TrogonPresence.Test.PresenceService do
  @moduledoc """
  Builds and runs a real, ephemeral `trogon-presence` service instance
  against a `TrogonPresence.Test.NatsServer`, so tests exercise the actual
  compiled binary rather than the in-process library the Rust suite itself
  uses for its own integration tests (`trogon_presence_service::start`,
  which Elixir cannot call into).

  Readiness is proven by a real `track` through `TrogonPresence.Writer`
  against a throwaway key and topic, retried until it stops timing out:
  the same call a caller makes, not a side channel.
  """

  alias TrogonPresence.Test.OsProcess
  alias TrogonPresence.{HolderId, Key, Meta, Topic, Writer}

  defstruct [:owner, :bucket]

  @type t :: %__MODULE__{owner: pid(), bucket: String.t()}

  @binary_env "TROGON_PRESENCE_BIN"
  @ready_attempts 100
  @ready_backoff 50

  @spec start!(String.t(), GenServer.server(), keyword()) :: t()
  def start!(nats_url, conn, extra_env \\ []) do
    binary = resolve_binary!()
    bucket = "presence_test_#{System.unique_integer([:positive])}"
    env = env(nats_url, bucket, extra_env)

    apply_buckets!(binary, env)

    {:ok, owner} = OsProcess.start_link(binary: binary, args: ["run"], env: env)
    service = %__MODULE__{owner: owner, bucket: bucket}
    await_ready!(service, conn)
    service
  end

  @spec stop(t()) :: :ok
  def stop(%__MODULE__{owner: owner}), do: OsProcess.stop(owner)

  defp apply_buckets!(binary, env) do
    case System.cmd(binary, ["bucket", "apply"], env: env, stderr_to_stdout: true) do
      {_output, 0} -> :ok
      {output, status} -> raise "trogon-presence bucket apply exited #{status}: #{output}"
    end
  end

  defp await_ready!(service, conn), do: await_ready!(service, conn, @ready_attempts)

  defp await_ready!(_service, _conn, 0) do
    raise "trogon-presence run did not become ready in time"
  end

  defp await_ready!(service, conn, attempts) do
    if OsProcess.exited?(service.owner) do
      raise "trogon-presence run exited before it became ready"
    end

    {:ok, key} = Key.new("readiness-probe")
    {:ok, topic} = Topic.new("readiness:probe")
    connection = TrogonPresence.ConnectionId.generate()
    holder = HolderId.generate()
    meta = Meta.new!(%{})

    case Writer.track(conn, key, connection, topic, holder, meta, timeout: 200) do
      {:ok, _written} -> :ok
      {:error, %TrogonPresence.Wire.ErrorReply{}} -> :ok
      {:error, _reason} -> await_ready!(service, conn, attempts - 1, @ready_backoff)
    end
  end

  defp await_ready!(service, conn, attempts, backoff) do
    Process.sleep(backoff)
    await_ready!(service, conn, attempts - 1)
  end

  defp env(nats_url, bucket, extra_env) do
    [
      {"TROGON_PRESENCE_NATS_URL", nats_url},
      {"TROGON_PRESENCE_BUCKET", bucket},
      {"TROGON_PRESENCE_LEASE_BUCKET", "#{bucket}_lease"},
      {"TROGON_PRESENCE_STORAGE", "memory"},
      {"TROGON_PRESENCE_LOG_LEVEL", "error"}
    ]
    |> Keyword.new(fn {key, value} -> {String.to_atom(key), value} end)
    |> Keyword.merge(Keyword.new(extra_env, fn {key, value} -> {String.to_atom(key), value} end))
    |> Enum.map(fn {key, value} -> {Atom.to_string(key), value} end)
  end

  defp resolve_binary! do
    System.get_env(@binary_env) || find_in_workspace!()
  end

  defp find_in_workspace! do
    root = workspace_root!(Path.absname(__DIR__))
    debug = Path.join([root, "target", "debug", "trogon-presence"])
    release = Path.join([root, "target", "release", "trogon-presence"])

    cond do
      File.exists?(debug) ->
        debug

      File.exists?(release) ->
        release

      true ->
        raise "no trogon-presence binary at #{debug} or #{release}; run `cargo build -p trogon_presence_service --bin trogon-presence` first, or set #{@binary_env}"
    end
  end

  defp workspace_root!(dir) do
    cargo_toml = Path.join(dir, "Cargo.toml")

    cond do
      File.exists?(cargo_toml) and String.contains?(File.read!(cargo_toml), "[workspace]") -> dir
      Path.dirname(dir) == dir -> raise "could not find the workspace root above #{__DIR__}"
      true -> workspace_root!(Path.dirname(dir))
    end
  end
end
