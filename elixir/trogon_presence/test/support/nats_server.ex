defmodule TrogonPresence.Test.NatsServer do
  @moduledoc """
  Spawns a real, ephemeral `nats-server` with JetStream enabled on a free
  loopback port, for tests to run `TrogonPresence.Writer` against the real
  system rather than a mock.

  Mirrors `trogon_presence_service/tests/common/mod.rs`: a dynamic free port,
  a disposable JetStream store directory, and readiness proven by actually
  connecting rather than by a fixed sleep.
  """

  alias TrogonPresence.Test.OsProcess

  defstruct [:owner, :url, :store_dir]

  @type t :: %__MODULE__{owner: pid(), url: String.t(), store_dir: Path.t()}

  @binary_env "TROGON_PRESENCE_NATS_SERVER"
  @ready_attempts 100
  @ready_backoff 50

  @spec start!() :: t()
  def start! do
    binary = resolve_binary!()

    store_dir =
      Path.join(System.tmp_dir!(), "trogon-presence-nats-#{System.unique_integer([:positive])}")

    File.mkdir_p!(store_dir)
    port = free_port!()

    {:ok, owner} =
      OsProcess.start_link(
        binary: binary,
        args: ["-js", "-a", "127.0.0.1", "-p", Integer.to_string(port), "-sd", store_dir]
      )

    server = %__MODULE__{owner: owner, url: "nats://127.0.0.1:#{port}", store_dir: store_dir}
    await_ready!(server, port)
    server
  end

  @spec stop(t()) :: :ok
  def stop(%__MODULE__{owner: owner, store_dir: store_dir}) do
    OsProcess.stop(owner)
    File.rm_rf(store_dir)
    :ok
  end

  defp await_ready!(server, port), do: await_ready!(server, port, @ready_attempts)

  defp await_ready!(_server, port, 0) do
    raise "nats-server on port #{port} did not become ready in time"
  end

  defp await_ready!(server, port, attempts) do
    if OsProcess.exited?(server.owner) do
      raise "nats-server on port #{port} exited before it became ready"
    end

    case :gen_tcp.connect(~c"127.0.0.1", port, [:binary, active: false], 200) do
      {:ok, socket} ->
        :gen_tcp.close(socket)

      {:error, _reason} ->
        Process.sleep(@ready_backoff)
        await_ready!(server, port, attempts - 1)
    end
  end

  defp free_port! do
    {:ok, socket} = :gen_tcp.listen(0, [:binary, active: false])
    {:ok, port} = :inet.port(socket)
    :gen_tcp.close(socket)
    port
  end

  defp resolve_binary! do
    System.get_env(@binary_env) || System.find_executable("nats-server") ||
      mise_which!("nats-server")
  end

  defp mise_which!(tool) do
    case System.cmd("mise", ["which", tool], stderr_to_stdout: true) do
      {path, 0} ->
        String.trim(path)

      {output, _status} ->
        raise "#{tool} is neither on PATH nor resolvable through `mise which #{tool}`: #{output}"
    end
  end
end
