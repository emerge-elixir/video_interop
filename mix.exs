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
      aliases: aliases(),
      package: package(),
      docs: docs(),
      deps: deps()
    ]
  end

  defp aliases do
    [test: [&test_without_schema_artifact/1]]
  end

  defp test_without_schema_artifact(args) do
    try do
      Mix.Tasks.Test.run(args)
    after
      [__DIR__, "priv", "native", "video_interop_schema_test.*"]
      |> Path.join()
      |> Path.wildcard()
      |> Enum.each(&File.rm/1)
    end
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
