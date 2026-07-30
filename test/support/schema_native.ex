defmodule VideoInterop.SchemaNative do
  @moduledoc false

  use Rustler,
    otp_app: :video_interop,
    crate: "video_interop_schema_test",
    path: "test/native/schema_test"

  def inspect_descriptor(_descriptor), do: :erlang.nif_error(:nif_not_loaded)
  def inspect_frame(_frame), do: :erlang.nif_error(:nif_not_loaded)
  def open_test_fd(), do: :erlang.nif_error(:nif_not_loaded)
  def prepare_and_drop_frame(_frame), do: :erlang.nif_error(:nif_not_loaded)
  def claim_and_drop_frame(_frame), do: :erlang.nif_error(:nif_not_loaded)
  def retire_frame(_frame), do: :erlang.nif_error(:nif_not_loaded)
end
