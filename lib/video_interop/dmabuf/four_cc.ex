defmodule VideoInterop.DMABuf.FourCC do
  @moduledoc """
  Helpers for DRM fourcc values.

  The canonical representation is the unsigned 32-bit integer used by DRM.
  Four-character binaries are encoded in little-endian byte order.
  """

  import Bitwise
  import Kernel, except: [to_string: 1]

  @max_u32 0xFFFF_FFFF

  @type t :: 0..0xFFFF_FFFF

  @spec from_string(binary()) :: {:ok, t()} | {:error, :invalid_fourcc}
  def from_string(<<a, b, c, d>>) do
    {:ok, a ||| b <<< 8 ||| c <<< 16 ||| d <<< 24}
  end

  def from_string(_value), do: {:error, :invalid_fourcc}

  @spec from_string!(binary()) :: t()
  def from_string!(value) do
    case from_string(value) do
      {:ok, fourcc} -> fourcc
      {:error, :invalid_fourcc} -> raise ArgumentError, "fourcc must contain exactly four bytes"
    end
  end

  @spec to_string(integer()) :: {:ok, <<_::32>>} | {:error, :invalid_fourcc}
  def to_string(fourcc) when is_integer(fourcc) and fourcc >= 0 and fourcc <= @max_u32 do
    {:ok,
     <<fourcc &&& 0xFF, fourcc >>> 8 &&& 0xFF, fourcc >>> 16 &&& 0xFF, fourcc >>> 24 &&& 0xFF>>}
  end

  def to_string(_value), do: {:error, :invalid_fourcc}

  @spec to_string!(integer()) :: <<_::32>>
  def to_string!(value) do
    case to_string(value) do
      {:ok, fourcc} ->
        fourcc

      {:error, :invalid_fourcc} ->
        raise ArgumentError, "fourcc must be an unsigned 32-bit integer"
    end
  end

  @spec nv12() :: t()
  def nv12, do: from_string!("NV12")

  @spec xrgb8888() :: t()
  def xrgb8888, do: from_string!("XR24")

  @spec p010() :: t()
  def p010, do: from_string!("P010")
end
