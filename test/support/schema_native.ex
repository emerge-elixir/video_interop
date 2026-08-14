defmodule VideoInterop.SchemaNative do
  @moduledoc false

  use Rustler,
    otp_app: :video_interop,
    crate: "video_interop_schema_test",
    path: "test/native/schema_test"

  def inspect_descriptor(_descriptor), do: :erlang.nif_error(:nif_not_loaded)
  def inspect_frame(_frame), do: :erlang.nif_error(:nif_not_loaded)
  def round_trip_format(_format), do: :erlang.nif_error(:nif_not_loaded)
  def round_trip_colorimetry(_colorimetry), do: :erlang.nif_error(:nif_not_loaded)
  def start_dispatcher(), do: :erlang.nif_error(:nif_not_loaded)
  def shutdown_dispatcher(_owner), do: :erlang.nif_error(:nif_not_loaded)
  def shutdown_dispatcher_timeout(_owner, _timeout_ms), do: :erlang.nif_error(:nif_not_loaded)
  def delay_dispatcher_for_test(_owner, _delay_ms), do: :erlang.nif_error(:nif_not_loaded)
  def dispatcher_health(_probe), do: :erlang.nif_error(:nif_not_loaded)

  @behaviour VideoInterop.AbandonmentGuard

  def new_abandonment_guard(dispatcher, owner, token, holder) do
    with {:ok, resource} <- new_abandonment_guard_resource(dispatcher, owner, token, holder) do
      {:ok, VideoInterop.AbandonmentGuard.new(resource, __MODULE__)}
    end
  end

  @impl true
  def video_interop_abandonment_guard?(resource), do: abandonment_guard_resource(resource)

  def new_abandonment_guard_resource(_dispatcher, _owner, _token, _holder),
    do: :erlang.nif_error(:nif_not_loaded)

  def abandonment_guard_resource(_resource), do: :erlang.nif_error(:nif_not_loaded)

  def fail_dispatcher_startup(), do: :erlang.nif_error(:nif_not_loaded)

  def fatal_enqueue_after_publication(_dispatcher, _owner, _token, _holder),
    do: :erlang.nif_error(:nif_not_loaded)

  def fatal_worker_panic(_dispatcher), do: :erlang.nif_error(:nif_not_loaded)
  def open_test_fd(), do: :erlang.nif_error(:nif_not_loaded)
  def prepare_and_drop_frame(_frame, _dispatcher), do: :erlang.nif_error(:nif_not_loaded)
  def claim_frame(_frame, _dispatcher), do: :erlang.nif_error(:nif_not_loaded)
  def claim_and_drop_frame(_frame, _dispatcher), do: :erlang.nif_error(:nif_not_loaded)
  def retire_frame(_frame, _dispatcher), do: :erlang.nif_error(:nif_not_loaded)
  def retire_claim(_claim), do: :erlang.nif_error(:nif_not_loaded)
end
