defmodule VideoInterop.FrameTest do
  use ExUnit.Case, async: true

  alias VideoInterop.{Colorimetry, Format, Frame, Lease, Rect, SyncFile}
  alias VideoInterop.DMABuf
  alias VideoInterop.DMABuf.{Descriptor, FourCC, Layer, Object, Plane}

  test "validates a frame against its format" do
    assert :ok = VideoInterop.validate(frame())
    assert :ok = VideoInterop.validate(format())
    assert :ok = VideoInterop.validate(frame(), format())
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

  test "rejects invalid interpretation metadata" do
    assert {:error, {:invalid_field, [:format, :interlace_mode], 123}} =
             VideoInterop.validate(%{format() | interlace_mode: 123})

    assert {:error, {:invalid_field, [:format, :colorimetry, :range], :bogus}} =
             VideoInterop.validate(%{format() | colorimetry: %Colorimetry{range: :bogus}})
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
