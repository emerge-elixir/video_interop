defmodule VideoInterop.FormatSchemaTest do
  use ExUnit.Case, async: true

  alias VideoInterop.{Colorimetry, Format, SchemaNative}
  alias VideoInterop.DMABuf
  alias VideoInterop.DMABuf.FourCC

  @primaries ~w(unspecified bt709 bt470_m bt470_bg smpte170m smpte240m film bt2020 smpte428 smpte431 smpte432 ebu3213)a
  @transfers ~w(unspecified bt709 gamma22 gamma28 smpte170m smpte240m linear log log_sqrt iec61966_2_4 bt1361 iec61966_2_1 bt2020_10 bt2020_12 smpte2084 smpte428 arib_std_b67)a
  @matrices ~w(unspecified rgb bt709 fcc bt470_bg smpte170m smpte240m ycgco bt2020_ncl bt2020_cl smpte2085 chroma_derived_ncl chroma_derived_cl ictcp)a
  @ranges ~w(unspecified limited full)a
  @chroma_locations ~w(unspecified left center top_left top bottom_left bottom)a

  test "round-trips the complete immutable stream schema exactly" do
    format = %Format{
      width: 3840,
      height: 2160,
      framerate: nil,
      storage: %DMABuf.Format{fourcc: FourCC.nv12(), modifier: 0xABCD_EF01_2345_6789},
      acquire_sync: :sync_file,
      colorimetry: %Colorimetry{
        primaries: :bt2020,
        transfer: :smpte2084,
        matrix: :bt2020_ncl,
        range: :limited,
        chroma_location: :top_left
      },
      pixel_aspect_ratio: {4, 3},
      interlace_mode: :interlaced_top_first,
      alpha_mode: :premultiplied
    }

    assert SchemaNative.round_trip_format(format) == {:ok, format}
  end

  test "round-trips every acquire and modifier policy without collapsing values" do
    for acquire_sync <- [:implicit, :sync_file, :per_frame],
        modifier <- [:per_buffer, :implicit, 0, 0xFFFF_FFFF_FFFF_FFFF] do
      format = %{
        format()
        | acquire_sync: acquire_sync,
          storage: %DMABuf.Format{fourcc: FourCC.nv12(), modifier: modifier}
      }

      assert SchemaNative.round_trip_format(format) == {:ok, format}
    end
  end

  test "round-trips every supported colorimetry value" do
    supported = [
      primaries: @primaries,
      transfer: @transfers,
      matrix: @matrices,
      range: @ranges,
      chroma_location: @chroma_locations
    ]

    Enum.each(supported, fn {field, values} ->
      Enum.each(values, fn value ->
        colorimetry = struct!(Colorimetry, [{field, value}])
        assert SchemaNative.round_trip_colorimetry(colorimetry) == colorimetry

        candidate = %{format() | colorimetry: colorimetry}
        assert SchemaNative.round_trip_format(candidate) == {:ok, candidate}
      end)
    end)
  end

  test "preserves explicitly unspecified colorimetry" do
    colorimetry = %Colorimetry{}
    assert SchemaNative.round_trip_colorimetry(colorimetry) == colorimetry

    candidate = %{format() | framerate: nil, colorimetry: colorimetry}
    assert SchemaNative.round_trip_format(candidate) == {:ok, candidate}
  end

  test "round-trips all interlace and alpha schema values" do
    for interlace_mode <- [
          :progressive,
          :interlaced_top_first,
          :interlaced_bottom_first,
          :mixed
        ],
        alpha_mode <- [:opaque, :straight, :premultiplied] do
      candidate = %{format() | interlace_mode: interlace_mode, alpha_mode: alpha_mode}
      assert SchemaNative.round_trip_format(candidate) == {:ok, candidate}
    end
  end

  test "rejects unsupported policy and colorimetry boundary values" do
    assert_native_rejection(fn ->
      SchemaNative.round_trip_format(%{format() | acquire_sync: :unsupported})
    end)

    assert_native_rejection(fn ->
      SchemaNative.round_trip_format(%{
        format()
        | storage: %DMABuf.Format{fourcc: FourCC.nv12(), modifier: :unsupported}
      })
    end)

    for field <- [:primaries, :transfer, :matrix, :range, :chroma_location] do
      assert_native_rejection(fn ->
        SchemaNative.round_trip_colorimetry(struct!(Colorimetry, [{field, :unsupported}]))
      end)
    end
  end

  test "rejects invalid numeric stream schema values" do
    assert SchemaNative.round_trip_format(%{format() | width: 0}) ==
             {:error, "stream format size must be positive, got 0x480"}

    assert SchemaNative.round_trip_format(%{format() | framerate: {60, 0}}) ==
             {:error, "stream framerate must be positive, got 60/0"}

    assert SchemaNative.round_trip_format(%{
             format()
             | storage: %DMABuf.Format{fourcc: 0, modifier: :per_buffer}
           }) == {:error, "stream format has invalid DRM fourcc 0"}

    assert SchemaNative.round_trip_format(%{format() | pixel_aspect_ratio: {0, 1}}) ==
             {:error, "pixel aspect ratio must be positive, got 0/1"}

    assert_native_rejection(fn ->
      SchemaNative.round_trip_format(%{
        format()
        | storage: %DMABuf.Format{fourcc: FourCC.nv12(), modifier: -1}
      })
    end)
  end

  defp assert_native_rejection(fun) do
    try do
      fun.()
      flunk("expected native schema decoding to reject the value")
    rescue
      error in [ArgumentError, ErlangError] -> error
    end
  end

  defp format do
    %Format{
      width: 640,
      height: 480,
      framerate: {60, 1},
      storage: %DMABuf.Format{fourcc: FourCC.nv12()},
      acquire_sync: :per_frame,
      colorimetry: %Colorimetry{},
      pixel_aspect_ratio: {1, 1},
      interlace_mode: :progressive,
      alpha_mode: :opaque
    }
  end
end
