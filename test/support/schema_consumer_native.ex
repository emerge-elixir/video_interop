defmodule VideoInterop.SchemaConsumerNative do
  @moduledoc false

  @on_load :load_nif
  @nif_path Path.expand(
              "../../target/schema-fixtures/video_interop_schema_consumer_test",
              __DIR__
            )

  def load_nif do
    :erlang.load_nif(String.to_charlist(@nif_path), 0)
  end

  def start_dispatcher(), do: :erlang.nif_error(:nif_not_loaded)
  def shutdown_dispatcher(_owner), do: :erlang.nif_error(:nif_not_loaded)
  def dispatcher_health(_probe), do: :erlang.nif_error(:nif_not_loaded)
  def guard_is_opaque_resource(_frame), do: :erlang.nif_error(:nif_not_loaded)
  def claim_frame(_frame, _dispatcher), do: :erlang.nif_error(:nif_not_loaded)
  def claim_and_drop_frame(_frame, _dispatcher), do: :erlang.nif_error(:nif_not_loaded)
  def retire_frame(_frame, _dispatcher), do: :erlang.nif_error(:nif_not_loaded)
  def retire_claim(_claim), do: :erlang.nif_error(:nif_not_loaded)
end
