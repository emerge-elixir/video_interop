defmodule VideoInterop.DMABuf.FourCCTest do
  use ExUnit.Case, async: true

  alias VideoInterop.DMABuf.FourCC

  test "round-trips DRM fourcc values" do
    assert {:ok, fourcc} = FourCC.from_string("NV12")
    assert fourcc == 0x3231_564E
    assert FourCC.to_string(fourcc) == {:ok, "NV12"}
    assert FourCC.nv12() == fourcc
  end

  test "rejects malformed values" do
    assert FourCC.from_string("RGB") == {:error, :invalid_fourcc}
    assert FourCC.to_string(-1) == {:error, :invalid_fourcc}
    assert_raise ArgumentError, fn -> FourCC.from_string!("too long") end
  end
end
