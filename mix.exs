defmodule VideoInterop.MixProject do
  use Mix.Project

  @version "0.1.0"

  def project do
    [
      app: :video_interop,
      version: @version,
      elixir: "~> 1.17",
      description: "Owned video frame descriptors, synchronization, and leases",
      elixirc_paths: elixirc_paths(Mix.env()),
      package: package(),
      docs: docs(),
      deps: deps()
    ]
  end

  defp deps do
    [
      {:rustler, "~> 0.38.0", only: :test, runtime: false},
      {:ex_doc, ">= 0.38.0", only: :dev, runtime: false}
    ]
  end

  defp elixirc_paths(:test), do: ["lib", "test/support"]
  defp elixirc_paths(_env), do: ["lib"]

  defp package do
    [
      licenses: ["Apache-2.0"],
      links: %{"GitHub" => "https://github.com/emerge-elixir/video_interop"},
      files: [
        "lib",
        "rust/video-interop/src",
        "rust/video-interop/Cargo.toml",
        "rust/video-interop/README.md",
        "rust/video-interop/LICENSE",
        "mix.exs",
        "README.md",
        "CHANGELOG.md",
        "LICENSE"
      ]
    ]
  end

  defp docs do
    [
      main: "readme",
      extras: ["README.md", "CHANGELOG.md", LICENSE: [title: "License"]]
    ]
  end
end
