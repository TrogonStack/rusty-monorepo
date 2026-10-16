defmodule TrogonPresence.Application do
  # See https://hexdocs.pm/elixir/Application.html
  # for more information on OTP Applications
  @moduledoc false

  use Application

  @impl true
  def start(_type, _args) do
    children = [
      # Starts a worker by calling: TrogonPresence.Worker.start_link(arg)
      # {TrogonPresence.Worker, arg}
    ]

    # See https://hexdocs.pm/elixir/Supervisor.html
    # for other strategies and supported options
    opts = [strategy: :one_for_one, name: TrogonPresence.Supervisor]
    Supervisor.start_link(children, opts)
  end
end
