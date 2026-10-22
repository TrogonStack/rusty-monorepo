defmodule TrogonPresence.Test.OsProcess do
  @moduledoc """
  Owns one externally spawned OS process (`nats-server`, `trogon-presence run`)
  for the duration of a test, discarding its stdout/stderr and guaranteeing it
  is actually killed on stop.

  `Port.close/1` alone does not kill a process started with
  `:spawn_executable`: closing the port only tears down the pipes, and
  neither `nats-server` nor `trogon-presence` exit on stdin EOF. Stopping
  this process instead signals the real OS pid directly.

  Traps exits so an abnormal exit of whatever process linked to this one
  (a crashing test, for example) still runs `terminate/2` instead of
  letting the BEAM kill this process outright and orphan the OS process.
  """

  use GenServer

  defstruct [:port, :os_pid, exit_status: nil]

  @type t :: %__MODULE__{
          port: port(),
          os_pid: non_neg_integer(),
          exit_status: non_neg_integer() | nil
        }

  @spec start_link(keyword()) :: GenServer.on_start()
  def start_link(opts) do
    GenServer.start_link(__MODULE__, opts)
  end

  @spec exited?(GenServer.server()) :: boolean()
  def exited?(pid), do: GenServer.call(pid, :exited?)

  @spec stop(GenServer.server()) :: :ok
  def stop(pid) do
    GenServer.call(pid, :stop)
  catch
    :exit, _ -> :ok
  end

  @impl true
  def init(opts) do
    Process.flag(:trap_exit, true)

    binary = Keyword.fetch!(opts, :binary)
    args = Keyword.get(opts, :args, [])
    env = Keyword.get(opts, :env, [])

    port =
      Port.open({:spawn_executable, String.to_charlist(binary)}, [
        :binary,
        :exit_status,
        :hide,
        args: Enum.map(args, &String.to_charlist/1),
        env:
          Enum.map(env, fn {key, value} ->
            {String.to_charlist(key), String.to_charlist(value)}
          end)
      ])

    os_pid = port |> Port.info() |> Keyword.fetch!(:os_pid)
    {:ok, %__MODULE__{port: port, os_pid: os_pid}}
  end

  @impl true
  def handle_call(:exited?, _from, state), do: {:reply, state.exit_status != nil, state}

  def handle_call(:stop, _from, %__MODULE__{} = state) do
    kill(state)
    {:stop, :normal, :ok, state}
  end

  @impl true
  def handle_info({port, {:data, _data}}, %__MODULE__{port: port} = state), do: {:noreply, state}

  def handle_info({port, {:exit_status, status}}, %__MODULE__{port: port} = state) do
    {:noreply, %{state | exit_status: status}}
  end

  def handle_info({:EXIT, _pid, _reason}, %__MODULE__{} = state), do: {:stop, :normal, state}

  @impl true
  def terminate(_reason, state), do: kill(state)

  defp kill(%__MODULE__{exit_status: status}) when not is_nil(status), do: :ok

  defp kill(%__MODULE__{port: port, os_pid: os_pid}) do
    System.cmd("kill", ["-TERM", Integer.to_string(os_pid)], stderr_to_stdout: true)
    wait_for_exit(os_pid, 50)
    if Port.info(port), do: Port.close(port)
  catch
    _, _ -> :ok
  end

  defp wait_for_exit(_os_pid, 0), do: :ok

  defp wait_for_exit(os_pid, attempts) do
    case System.cmd("kill", ["-0", Integer.to_string(os_pid)], stderr_to_stdout: true) do
      {_output, 0} ->
        Process.sleep(20)
        wait_for_exit(os_pid, attempts - 1)

      {_output, _nonzero} ->
        :ok
    end
  end
end
