defmodule VideoInterop.SchemaConsumerNative do
  @moduledoc false

  use Rustler,
    otp_app: :video_interop,
    crate: "video_interop_schema_consumer_test",
    path: "test/native/schema_consumer_test"

  def start_dispatcher(), do: :erlang.nif_error(:nif_not_loaded)
  def shutdown_dispatcher(_owner), do: :erlang.nif_error(:nif_not_loaded)
  def dispatcher_health(_probe), do: :erlang.nif_error(:nif_not_loaded)
  def guard_is_opaque_resource(_frame), do: :erlang.nif_error(:nif_not_loaded)
  def claim_frame(_frame, _dispatcher), do: :erlang.nif_error(:nif_not_loaded)
  def claim_and_drop_frame(_frame, _dispatcher), do: :erlang.nif_error(:nif_not_loaded)
  def retire_frame(_frame, _dispatcher), do: :erlang.nif_error(:nif_not_loaded)
  def retire_claim(_claim), do: :erlang.nif_error(:nif_not_loaded)
end
