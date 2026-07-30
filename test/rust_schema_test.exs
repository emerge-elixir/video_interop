defmodule VideoInterop.RustSchemaTest do
  use ExUnit.Case, async: true

  alias VideoInterop.{Frame, Lease, Rect, SchemaNative, SyncFile}
  alias VideoInterop.DMABuf.{Descriptor, FourCC, Layer, Object, Plane}

  test "Rustler decodes the published descriptor schema" do
    assert SchemaNative.inspect_descriptor(descriptor(10)) ==
             {:ok, {1, 1, 1, FourCC.nv12(), 0, 2}}
  end

  test "Rustler decodes frame, acquire synchronization, and lifetime fields" do
    frame = frame(10, %SyncFile{acquire_fence_fd: 10})

    assert SchemaNative.inspect_frame(frame) == {:ok, {640, 480, 640, 480, true, true}}
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
    token = make_ref()
    lease = Lease.new(self(), token)

    assert SchemaNative.prepare_and_drop_frame(frame(fd, :implicit, lease)) == {:ok, true}
    refute_receive {:video_interop_release, ^token, _holder}, 50
    assert is_reference(resource)
  end

  test "claiming native ownership releases the lease when the frame drops" do
    assert {:ok, {fd, resource}} = SchemaNative.open_test_fd()
    token = make_ref()
    lease = Lease.new(self(), token)
    frame = frame(fd, :implicit, lease)

    assert SchemaNative.claim_and_drop_frame(frame) == {:ok, true}
    assert_receive {:video_interop_release, ^token, holder}, 1_000
    assert holder == lease.holder
    assert is_reference(resource)
  end

  test "explicit native retirement releases exactly once" do
    assert {:ok, {fd, resource}} = SchemaNative.open_test_fd()
    token = make_ref()
    lease = Lease.new(self(), token)

    assert SchemaNative.retire_frame(frame(fd, :implicit, lease)) == {:ok, true}
    assert_receive {:video_interop_release, ^token, holder}, 1_000
    assert holder == lease.holder
    refute_receive {:video_interop_release, ^token, _holder}, 50
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
