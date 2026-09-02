defmodule VideoInterop.RustSchemaTest do
  use ExUnit.Case, async: true

  alias VideoInterop.{Frame, Lease, Rect, SchemaConsumerNative, SchemaNative, SyncFile}
  alias VideoInterop.DMABuf.{Descriptor, FourCC, Layer, Object, Plane}

  test "test NIFs are isolated from the application's priv directory" do
    native_dir = Application.app_dir(:video_interop, "priv/native")

    assert Path.wildcard(Path.join(native_dir, "video_interop_schema_*test.*")) == []
  end

  test "Rustler decodes the published descriptor schema" do
    assert SchemaNative.inspect_descriptor(descriptor(10)) ==
             {:ok, {1, 1, 1, FourCC.nv12(), 0, 2}}
  end

  test "Rustler decodes frame, acquire synchronization, and lifetime fields" do
    frame = frame(10, %SyncFile{acquire_fence_fd: 10})

    assert SchemaNative.inspect_frame(frame) ==
             {:ok, {640, 480, 640, 480, true, true, false}}
  end

  test "Rustler decodes owned binary storage without a lease" do
    frame =
      Frame.binary(<<1, 2, 3, 4, 5, 6, 7, 8, 9, 10>>,
        width: 1,
        height: 2,
        stride: 6,
        pixel_format: :rgba8888
      )

    assert SchemaNative.inspect_binary_frame(frame) ==
             {:ok, {1, 2, 10, 0, 6, true, true}}
  end

  test "native preparation accepts owned binary frames without lease clients" do
    assert {:ok, {dispatcher, _probe}} = SchemaNative.start_dispatcher()

    frame =
      Frame.binary(<<1, 2, 3, 4>>, width: 1, height: 1, pixel_format: :rgba8888)

    assert SchemaNative.claim_and_drop_frame(frame, dispatcher) == {:ok, true}
    assert SchemaNative.shutdown_dispatcher(dispatcher) == {:ok, true}
  end

  test "native preparation rejects leases on owned binary frames" do
    assert {:ok, {dispatcher, _probe}} = SchemaNative.start_dispatcher()

    frame =
      Frame.binary(<<1, 2, 3, 4>>, width: 1, height: 1, pixel_format: :rgba8888)

    assert SchemaNative.claim_and_drop_frame(
             %{frame | lease: Lease.new(self(), make_ref())},
             dispatcher
           ) == {:error, "owned binary video frame storage must not carry a lease"}

    assert SchemaNative.shutdown_dispatcher(dispatcher) == {:ok, true}
  end

  test "Rustler rejects oversized AVDRM lists before decoding all entries" do
    descriptor = descriptor(10)
    [object] = descriptor.objects

    assert_raise ArgumentError, fn ->
      SchemaNative.inspect_descriptor(%{descriptor | objects: List.duplicate(object, 5)})
    end
  end

  test "dropping a prepared frame closes fds without releasing the caller's lease" do
    assert {:ok, {fd, resource}} = SchemaNative.open_test_fd()
    assert {:ok, {dispatcher, _probe}} = SchemaNative.start_dispatcher()
    on_exit(fn -> assert SchemaNative.shutdown_dispatcher(dispatcher) == {:ok, true} end)
    token = make_ref()
    lease = Lease.new(self(), token)

    assert SchemaNative.prepare_and_drop_frame(frame(fd, :implicit, lease), dispatcher) ==
             {:ok, true}

    refute_receive {:video_interop_release, ^token, _holder}, 50
    assert is_reference(resource)
  end

  test "claiming native ownership releases the lease when the frame drops" do
    assert {:ok, {fd, resource}} = SchemaNative.open_test_fd()
    assert {:ok, {dispatcher, _probe}} = SchemaNative.start_dispatcher()
    on_exit(fn -> assert SchemaNative.shutdown_dispatcher(dispatcher) == {:ok, true} end)
    token = make_ref()
    lease = Lease.new(self(), token)
    frame = frame(fd, :implicit, lease)

    assert SchemaNative.claim_and_drop_frame(frame, dispatcher) == {:ok, true}
    assert_receive {:video_interop_release, ^token, holder}, 1_000
    assert holder == lease.holder
    assert is_reference(resource)
  end

  test "dead release recipients are counted without failing the dispatcher" do
    assert {:ok, {fd, resource}} = SchemaNative.open_test_fd()
    assert {:ok, {dispatcher, probe}} = SchemaNative.start_dispatcher()
    owner = spawn(fn -> :ok end)
    monitor = Process.monitor(owner)
    assert_receive {:DOWN, ^monitor, :process, ^owner, _reason}

    lease = Lease.new(owner, make_ref())

    assert SchemaNative.claim_and_drop_frame(frame(fd, :implicit, lease), dispatcher) ==
             {:ok, true}

    eventually(fn -> SchemaNative.dispatcher_undelivered_commands(probe) == 1 end)
    assert SchemaNative.dispatcher_health(probe) == "healthy"
    assert SchemaNative.shutdown_dispatcher(dispatcher) == {:ok, true}
    assert is_reference(resource)
  end

  test "explicit native retirement releases exactly once" do
    assert {:ok, {fd, resource}} = SchemaNative.open_test_fd()
    assert {:ok, {dispatcher, _probe}} = SchemaNative.start_dispatcher()
    on_exit(fn -> assert SchemaNative.shutdown_dispatcher(dispatcher) == {:ok, true} end)
    token = make_ref()
    lease = Lease.new(self(), token)

    assert SchemaNative.retire_frame(frame(fd, :implicit, lease), dispatcher) == {:ok, true}
    assert_receive {:video_interop_release, ^token, holder}, 1_000
    assert holder == lease.holder
    refute_receive {:video_interop_release, ^token, _holder}, 50
    assert is_reference(resource)
  end

  test "close waits for admitted claims, rejects new admission, and can be retried" do
    assert {:ok, {fd, resource}} = SchemaNative.open_test_fd()
    assert {:ok, {dispatcher, probe}} = SchemaNative.start_dispatcher()
    lease = Lease.new(self(), make_ref())
    frame = frame(fd, :implicit, lease)
    assert {:ok, claim} = SchemaNative.claim_frame(frame, dispatcher)

    on_exit(fn ->
      SchemaNative.retire_claim(claim)
      SchemaNative.shutdown_dispatcher(dispatcher)
    end)

    assert {:error,
            "video-interop release dispatcher unavailable: timed out waiting for dispatcher clients to retire"} =
             SchemaNative.shutdown_dispatcher_timeout(dispatcher, 10)

    assert SchemaNative.dispatcher_health(probe) == "stopping"

    assert {:error, "video-interop release dispatcher unavailable: dispatcher is Stopping"} =
             SchemaNative.claim_frame(frame, dispatcher)

    assert SchemaNative.retire_claim(claim) == {:ok, true}
    assert_receive {:video_interop_release, token, holder}, 1_000
    assert token == lease.token
    assert holder == lease.holder
    assert SchemaNative.shutdown_dispatcher(dispatcher) == {:ok, true}
    assert SchemaNative.dispatcher_health(probe) == "stopped"
    assert is_reference(resource)
  end

  test "admission racing close either owns a counted client or is rejected" do
    assert {:ok, {fd, resource}} = SchemaNative.open_test_fd()

    Enum.each(1..100, fn _iteration ->
      assert {:ok, {dispatcher, probe}} = SchemaNative.start_dispatcher()
      lease = Lease.new(self(), make_ref())
      candidate = frame(fd, :implicit, lease)

      claim_task =
        Task.async(fn ->
          receive do
            :race -> SchemaNative.claim_frame(candidate, dispatcher)
          end
        end)

      close_task =
        Task.async(fn ->
          receive do
            :race -> SchemaNative.shutdown_dispatcher_timeout(dispatcher, 20)
          end
        end)

      send(claim_task.pid, :race)
      send(close_task.pid, :race)
      claim_result = Task.await(claim_task, 1_000)
      close_result = Task.await(close_task, 1_000)

      case claim_result do
        {:ok, claim} ->
          assert close_result ==
                   {:error,
                    "video-interop release dispatcher unavailable: timed out waiting for dispatcher clients to retire"}

          assert SchemaNative.retire_claim(claim) == {:ok, true}
          assert_receive {:video_interop_release, token, holder}, 1_000
          assert token == lease.token
          assert holder == lease.holder
          assert SchemaNative.shutdown_dispatcher(dispatcher) == {:ok, true}

        {:error, reason} ->
          assert reason in [
                   "video-interop release dispatcher unavailable: dispatcher is Stopping",
                   "video-interop release dispatcher unavailable: dispatcher is Stopped"
                 ]

          assert close_result == {:ok, true}
      end

      assert SchemaNative.dispatcher_health(probe) == "stopped"
    end)

    assert is_reference(resource)
  end

  test "the shared crate registers resources in separate producer and consumer NIFs" do
    assert {:ok, {producer_dispatcher, _probe}} = SchemaNative.start_dispatcher()

    assert {:ok, {consumer_dispatcher, _consumer_probe}} =
             SchemaConsumerNative.start_dispatcher()

    on_exit(fn ->
      assert SchemaConsumerNative.shutdown_dispatcher(consumer_dispatcher) == {:ok, true}
      assert SchemaNative.shutdown_dispatcher(producer_dispatcher) == {:ok, true}
    end)

    assert {:ok, {fd, resource}} = SchemaNative.open_test_fd()

    owner = self()
    token = make_ref()
    holder = make_ref()

    assert {:ok, guard} =
             SchemaNative.new_abandonment_guard(producer_dispatcher, owner, token, holder)

    lease = %Lease{
      owner: owner,
      token: token,
      holder: holder,
      abandonment_guard: guard
    }

    assert VideoInterop.AbandonmentGuard.valid?(guard)
    guarded_frame = frame(fd, :implicit, lease)
    assert SchemaConsumerNative.guard_is_opaque_resource(guarded_frame)

    assert SchemaConsumerNative.claim_and_drop_frame(guarded_frame, consumer_dispatcher) ==
             {:ok, true}

    assert_receive {:video_interop_release, ^token, ^holder}, 1_000
    assert is_reference(resource)
  end

  defp frame(fd, acquire_sync, lease \\ Lease.new(self(), make_ref())) do
    %Frame{
      coded_width: 640,
      coded_height: 480,
      visible_rect: %Rect{x: 0, y: 0, width: 640, height: 480},
      storage: descriptor(fd),
      acquire_sync: acquire_sync,
      lease: lease
    }
  end

  defp eventually(assertion, attempts \\ 100)
  defp eventually(assertion, 0), do: assert(assertion.())

  defp eventually(assertion, attempts) do
    if assertion.() do
      :ok
    else
      Process.sleep(1)
      eventually(assertion, attempts - 1)
    end
  end

  defp descriptor(fd) do
    %Descriptor{
      objects: [%Object{fd: fd, size: 460_800, modifier: 0}],
      layers: [
        %Layer{
          fourcc: FourCC.nv12(),
          planes: [
            %Plane{object_index: 0, offset: 0, pitch: 640},
            %Plane{object_index: 0, offset: 307_200, pitch: 640}
          ]
        }
      ]
    }
  end
end
