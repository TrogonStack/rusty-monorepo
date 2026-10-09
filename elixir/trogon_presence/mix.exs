defmodule TrogonPresence.MixProject do
  use Mix.Project

  def project do
    [
      app: :trogon_presence,
      version: "0.1.0",
      elixir: "~> 1.20",
      start_permanent: Mix.env() == :prod,
      deps: deps()
    ]
  end

  # Run "mix help compile.app" to learn about applications.
  def application do
    [
      extra_applications: [:logger],
      mod: {TrogonPresence.Application, []}
    ]
  end

  # Run "mix help deps" to learn about dependencies.
  defp deps do
    [
      {:gnat, "~> 1.9"},
      {:jason, "~> 1.4"},
      {:phoenix_pubsub, "~> 2.1"},
      {:phoenix, "~> 1.7", only: [:dev, :test]},
      {:phoenix_html, "~> 4.0", only: :test},
      {:plug_cowboy, "~> 2.7", only: :test}
    ]
  end
end
