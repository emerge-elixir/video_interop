defmodule VideoInterop.Frame do
  @moduledoc """
  One video frame backed by an owned BEAM binary or borrowed DMA-BUF storage.

  Binary storage owns its immutable bytes and has no lease. DMA-BUF storage
  carries synchronization metadata and a producer lifetime lease. Timestamps
  belong to the transport using the frame.
  """

  alias VideoInterop.{Binary, Format, Lease, Rect, SyncFile}
  alias VideoInterop.Binary.Plane
  alias VideoInterop.DMABuf.Descriptor

  @enforce_keys [:coded_width, :coded_height, :visible_rect, :storage]
  defstruct coded_width: nil,
            coded_height: nil,
            visible_rect: nil,
            format: nil,
            storage: nil,
            acquire_sync: :implicit,
            lease: nil

  @type storage :: Binary.t() | Descriptor.t()
  @type acquire_sync :: :implicit | SyncFile.t()

  @type t :: %__MODULE__{
          coded_width: pos_integer(),
          coded_height: pos_integer(),
          visible_rect: Rect.t(),
          format: Format.t() | nil,
          storage: storage(),
          acquire_sync: acquire_sync(),
          lease: Lease.t() | nil
        }

  @doc "Builds a validated, tightly packed or explicitly strided binary frame."
  @spec binary(binary(), keyword()) :: t()
  def binary(data, opts) when is_binary(data) and is_list(opts) do
    width = Keyword.fetch!(opts, :width)
    height = Keyword.fetch!(opts, :height)
    pixel_format = Keyword.fetch!(opts, :pixel_format)
    stride = Keyword.get_lazy(opts, :stride, fn -> minimum_stride!(width, pixel_format) end)
    polarity = Keyword.get(opts, :bw1_polarity)

    format = %Format{
      width: width,
      height: height,
      framerate: Keyword.get(opts, :framerate),
      storage: %Binary.Format{pixel_format: pixel_format, bw1_polarity: polarity},
      acquire_sync: :implicit,
      alpha_mode: Keyword.get(opts, :alpha_mode, default_alpha_mode(pixel_format))
    }

    frame = %__MODULE__{
      coded_width: width,
      coded_height: height,
      visible_rect: %Rect{x: 0, y: 0, width: width, height: height},
      format: format,
      storage: %Binary{data: data, planes: [%Plane{offset: 0, stride: stride}]}
    }

    case VideoInterop.validate(frame) do
      :ok -> frame
      {:error, reason} -> raise ArgumentError, "invalid binary video frame: #{inspect(reason)}"
    end
  end

  defp minimum_stride!(width, :rgba8888) when is_integer(width) and width > 0, do: width * 4
  defp minimum_stride!(width, :rgb888) when is_integer(width) and width > 0, do: width * 3
  defp minimum_stride!(width, :gray8) when is_integer(width) and width > 0, do: width
  defp minimum_stride!(width, :gray2) when is_integer(width) and width > 0, do: div(width + 3, 4)
  defp minimum_stride!(width, :bw1) when is_integer(width) and width > 0, do: div(width + 7, 8)

  defp minimum_stride!(width, pixel_format) do
    raise ArgumentError,
          "unsupported binary pixel format or width: #{inspect({pixel_format, width})}"
  end

  defp default_alpha_mode(:rgba8888), do: :premultiplied
  defp default_alpha_mode(_pixel_format), do: :opaque
end
