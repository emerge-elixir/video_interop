defmodule VideoInterop.FrameTest do
  use ExUnit.Case, async: true

  alias VideoInterop.{Binary, Colorimetry, Format, Frame, Lease, LeaseOwner, Rect, SyncFile}
  alias VideoInterop.DMABuf
  alias VideoInterop.DMABuf.{Descriptor, FourCC, Layer, Object, Plane}

  test "validates a frame against its format" do
    assert :ok = VideoInterop.validate(frame())
    assert :ok = VideoInterop.validate(format())
    assert :ok = VideoInterop.validate(frame(), format())
  end

  test "validates an owned binary frame without a lease" do
    frame =
      Frame.binary(<<0, 1, 2, 3, 4, 5>>, width: 2, height: 1, pixel_format: :rgb888)

    assert :ok = VideoInterop.validate(frame)

    assert %Binary{data: <<0, 1, 2, 3, 4, 5>>, planes: [%Binary.Plane{stride: 6}]} =
             frame.storage

    assert frame.acquire_sync == :implicit
    assert frame.format.acquire_sync == :implicit
    assert frame.lease == nil
    assert {:ok, ^frame} = VideoInterop.retain(frame)
    assert :ok = VideoInterop.release(frame)
  end

  test "validates packed grayscale polarity and row stride" do
    assert :ok =
             VideoInterop.validate(
               Frame.binary(<<0x80, 0x00>>,
                 width: 9,
                 height: 1,
                 pixel_format: :bw1,
                 bw1_polarity: :one_is_white
               )
             )

    assert_raise ArgumentError, fn ->
      Frame.binary(<<0>>, width: 9, height: 1, pixel_format: :bw1, bw1_polarity: :one_is_white)
    end
  end

  test "rejects non-implicit synchronization for binary formats" do
    frame = Frame.binary(<<0>>, width: 1, height: 1, pixel_format: :gray8)

    assert {:error, {:binary_format_requires_implicit_sync, :per_frame}} =
             VideoInterop.validate(%{frame.format | acquire_sync: :per_frame})
  end

  test "rejects alpha modes on binary formats without alpha" do
    frame = Frame.binary(<<0>>, width: 1, height: 1, pixel_format: :gray8)

    assert {:error, {:binary_format_requires_opaque_alpha, :straight}} =
             VideoInterop.validate(%{frame.format | alpha_mode: :straight})
  end

  test "rejects binary pixel interpretation mismatches" do
    frame = Frame.binary(<<0, 1, 2>>, width: 1, height: 1, pixel_format: :rgb888)
    expected = %{frame.format | storage: %Binary.Format{pixel_format: :gray8}}

    assert {:error, {:binary_format_mismatch, _, _}} =
             VideoInterop.validate(frame, expected)
  end

  test "allows an unknown framerate" do
    assert :ok = VideoInterop.validate(%{format() | framerate: nil})
  end

  test "rejects visible rectangles outside coded dimensions" do
    frame = %{frame() | visible_rect: %Rect{x: 1, y: 0, width: 640, height: 480}}

    assert {:error, {:visible_rect_out_of_bounds, _rect, {640, 480}}} =
             VideoInterop.validate(frame)
  end

  test "validates borrowed acquire fences" do
    assert :ok =
             VideoInterop.validate(%{
               frame()
               | acquire_sync: %SyncFile{acquire_fence_fd: 20}
             })

    assert {:error, {:invalid_acquire_sync, :unsupported}} =
             VideoInterop.validate(%{frame() | acquire_sync: :unsupported})
  end

  test "enforces the negotiated acquire synchronization policy" do
    implicit = frame()
    explicit = %{implicit | acquire_sync: %SyncFile{acquire_fence_fd: 20}}

    assert :ok = VideoInterop.validate(implicit, %{format() | acquire_sync: :implicit})
    assert :ok = VideoInterop.validate(explicit, %{format() | acquire_sync: :sync_file})
    assert :ok = VideoInterop.validate(implicit, %{format() | acquire_sync: :per_frame})
    assert :ok = VideoInterop.validate(explicit, %{format() | acquire_sync: :per_frame})

    assert {:error, {:acquire_sync_mismatch, :implicit, :sync_file}} =
             VideoInterop.validate(implicit, %{format() | acquire_sync: :sync_file})

    assert {:error, {:acquire_sync_mismatch, :sync_file, :implicit}} =
             VideoInterop.validate(explicit, %{format() | acquire_sync: :implicit})
  end

  test "rejects invalid interpretation metadata" do
    assert {:error, {:invalid_field, [:format, :interlace_mode], 123}} =
             VideoInterop.validate(%{format() | interlace_mode: 123})

    assert {:error, {:invalid_field, [:format, :colorimetry, :range], :bogus}} =
             VideoInterop.validate(%{format() | colorimetry: %Colorimetry{range: :bogus}})

    assert {:error, {:invalid_field, [:format, :acquire_sync], :bogus}} =
             VideoInterop.validate(%{format() | acquire_sync: :bogus})
  end

  test "retains a frame with identical data and a distinct holder" do
    test_pid = self()

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        release: fn token ->
          send(test_pid, {:backend_released, token})
          :ok
        end
      )

    assert {:ok, root} = LeaseOwner.issue(owner, :surface)
    original = %{frame() | lease: root}
    assert {:ok, child} = VideoInterop.retain(original)

    assert %{child | lease: original.lease} == original
    assert child.lease.owner == original.lease.owner
    assert child.lease.token == original.lease.token
    assert child.lease.holder != original.lease.holder

    assert :ok = VideoInterop.release(original)
    refute_receive {:backend_released, :surface}
    assert :ok = VideoInterop.release(child)
    assert_receive {:backend_released, :surface}
    assert :ok = LeaseOwner.close(owner)
  end

  test "detects format mismatches" do
    assert {:error, {:coded_size_mismatch, {640, 480}, {1280, 720}}} =
             VideoInterop.validate(frame(), %{format() | width: 1280, height: 720})

    assert {:error, {:fourcc_mismatch, _, _}} =
             VideoInterop.validate(frame(), %{
               format()
               | storage: %DMABuf.Format{fourcc: FourCC.xrgb8888()}
             })
  end

  defp format do
    %Format{
      width: 640,
      height: 480,
      framerate: {60, 1},
      storage: %DMABuf.Format{fourcc: FourCC.nv12()}
    }
  end

  defp frame do
    %Frame{
      coded_width: 640,
      coded_height: 480,
      visible_rect: %Rect{x: 0, y: 0, width: 640, height: 480},
      storage: %Descriptor{
        objects: [%Object{fd: 10, size: 460_800, modifier: :implicit}],
        layers: [
          %Layer{
            fourcc: FourCC.nv12(),
            planes: [
              %Plane{object_index: 0, offset: 0, pitch: 640},
              %Plane{object_index: 0, offset: 307_200, pitch: 640}
            ]
          }
        ]
      },
      lease: Lease.new(self(), make_ref())
    }
  end
end
