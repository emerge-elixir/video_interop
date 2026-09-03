defmodule VideoInterop.MixProject do
  use Mix.Project

  @version "0.1.0"

  def project do
    [
      app: :video_interop,
      version: @version,
      elixir: "~> 1.17",
      description: "Video frame, DMA-BUF, synchronization, and ownership primitives",
      source_url: "https://github.com/emerge-elixir/video_interop",
      homepage_url: "https://github.com/emerge-elixir/video_interop",
      elixirc_paths: elixirc_paths(Mix.env()),
      aliases: aliases(),
      package: package(),
      docs: docs(),
      deps: deps()
    ]
  end

  defp aliases do
    [test: [&test_with_schema_artifacts/1]]
  end

  defp test_with_schema_artifacts(args) do
    stage_schema_fixtures!()
    Mix.Tasks.Test.run(args)
  end

  defp stage_schema_fixtures! do
    fixtures = ["video_interop_schema_test", "video_interop_schema_consumer_test"]
    cargo_args = ["build", "--release"] ++ Enum.flat_map(fixtures, &["-p", &1])
    target_dir = Path.join(__DIR__, "target")
    fixture_dir = Path.join(target_dir, "schema-fixtures")

    remove_legacy_schema_fixtures!()

    case System.cmd("cargo", cargo_args,
           cd: __DIR__,
           env: [{"CARGO_TARGET_DIR", target_dir}],
           stderr_to_stdout: true
         ) do
      {_output, 0} -> :ok
      {output, status} -> Mix.raise("schema fixture build failed (#{status}):\n#{output}")
    end

    File.mkdir_p!(fixture_dir)

    Enum.each(fixtures, fn fixture ->
      source = Path.join([target_dir, "release", cargo_library_name(fixture)])
      destination = Path.join(fixture_dir, nif_library_name(fixture))
      File.rm(destination)
      File.cp!(source, destination)
    end)
  end

  defp remove_legacy_schema_fixtures! do
    [__DIR__, "priv", "native", "video_interop_schema_*test.*"]
    |> Path.join()
    |> Path.wildcard()
    |> Enum.each(&File.rm!/1)
  end

  defp cargo_library_name(fixture) do
    case :os.type() do
      {:win32, _} -> "#{fixture}.dll"
      {:unix, :darwin} -> "lib#{fixture}.dylib"
      {:unix, _} -> "lib#{fixture}.so"
    end
  end

  defp nif_library_name(fixture) do
    case :os.type() do
      {:win32, _} -> "#{fixture}.dll"
      {:unix, _} -> "#{fixture}.so"
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
      source_url: "https://github.com/emerge-elixir/video_interop",
      source_ref: "v#{@version}",
      extras: ["README.md", "CHANGELOG.md", LICENSE: [title: "License"]]
    ]
  end
end
