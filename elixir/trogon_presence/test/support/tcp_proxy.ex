defmodule TrogonPresence.Test.TcpProxy do
  @moduledoc """
  A loopback TCP relay in front of a real `nats-server`, so a test can sever
  one client's network path while the server and every other client keep
  running. `cut/1` drops every relayed connection and stops listening, so a
  reconnect attempt is refused outright, until `restore/1` listens again on
  the same port.
  """

  use GenServer

  defstruct [:port, :upstream_port, :listen, :acceptor, relays: MapSet.new()]

  @type t :: pid()

  @spec start_link(:inet.port_number()) :: GenServer.on_start()
  def start_link(upstream_port), do: GenServer.start_link(__MODULE__, upstream_port)

  @spec port(t()) :: :inet.port_number()
  def port(proxy), do: GenServer.call(proxy, :port)

  @spec cut(t()) :: :ok
  def cut(proxy), do: GenServer.call(proxy, :cut)

  @spec restore(t()) :: :ok
  def restore(proxy), do: GenServer.call(proxy, :restore)

  @impl GenServer
  def init(upstream_port) do
    Process.flag(:trap_exit, true)
    {:ok, listen(%__MODULE__{port: 0, upstream_port: upstream_port})}
  end

  @impl GenServer
  def handle_call(:port, _from, state), do: {:reply, state.port, state}

  def handle_call(:cut, _from, state), do: {:reply, :ok, close(state)}

  def handle_call(:restore, _from, %{listen: nil} = state), do: {:reply, :ok, listen(state)}
  def handle_call(:restore, _from, state), do: {:reply, :ok, state}

  @impl GenServer
  def handle_info({:accepted, client}, %{listen: nil} = state) do
    :gen_tcp.close(client)
    {:noreply, state}
  end

  def handle_info({:accepted, client}, state) do
    upstream_port = state.upstream_port
    relay = spawn_link(fn -> relay(client, upstream_port) end)
    :ok = :gen_tcp.controlling_process(client, relay)
    send(relay, :go)
    {:noreply, %{state | relays: MapSet.put(state.relays, relay)}}
  end

  def handle_info({:EXIT, pid, _reason}, state) do
    {:noreply, %{state | relays: MapSet.delete(state.relays, pid)}}
  end

  @impl GenServer
  def terminate(_reason, state) do
    close(state)
    :ok
  end

  defp listen(state) do
    {:ok, listen} =
      :gen_tcp.listen(state.port, [:binary, active: false, reuseaddr: true, ip: {127, 0, 0, 1}])

    {:ok, port} = :inet.port(listen)
    proxy = self()
    acceptor = spawn_link(fn -> accept_loop(listen, proxy) end)
    %{state | listen: listen, port: port, acceptor: acceptor}
  end

  defp close(%{listen: nil} = state), do: state

  defp close(state) do
    :gen_tcp.close(state.listen)
    Process.exit(state.acceptor, :kill)
    Enum.each(state.relays, &Process.exit(&1, :kill))
    %{state | listen: nil, acceptor: nil, relays: MapSet.new()}
  end

  defp accept_loop(listen, proxy) do
    case :gen_tcp.accept(listen) do
      {:ok, client} ->
        :ok = :gen_tcp.controlling_process(client, proxy)
        send(proxy, {:accepted, client})
        accept_loop(listen, proxy)

      {:error, _reason} ->
        :ok
    end
  end

  defp relay(client, upstream_port) do
    receive do
      :go -> :ok
    end

    case :gen_tcp.connect(~c"127.0.0.1", upstream_port, [:binary, active: true, nodelay: true]) do
      {:ok, upstream} ->
        :ok = :inet.setopts(client, active: true, nodelay: true)
        pump(client, upstream)

      {:error, _reason} ->
        :gen_tcp.close(client)
    end
  end

  defp pump(client, upstream) do
    receive do
      {:tcp, ^client, data} ->
        :gen_tcp.send(upstream, data)
        pump(client, upstream)

      {:tcp, ^upstream, data} ->
        :gen_tcp.send(client, data)
        pump(client, upstream)

      {:tcp_closed, _socket} ->
        :gen_tcp.close(client)
        :gen_tcp.close(upstream)

      {:tcp_error, _socket, _reason} ->
        :gen_tcp.close(client)
        :gen_tcp.close(upstream)
    end
  end
end
