defmodule VideoInterop.Format do
  @moduledoc """
  Framework-neutral video format associated with a sequence of frames.

  `framerate: nil` represents an unknown cadence. The storage-specific format
  is held in `storage`, allowing future non-DMA-BUF storage without changing
  the outer video interpretation fields.
  """

  alias VideoInterop.Colorimetry
  alias VideoInterop.DMABuf

  @enforce_keys [:width, :height, :framerate, :storage]
  defstruct width: nil,
            height: nil,
            framerate: nil,
            storage: nil,
            colorimetry: %Colorimetry{},
            pixel_aspect_ratio: {1, 1},
            interlace_mode: :progressive,
            alpha_mode: :opaque

  @type interlace_mode ::
          :progressive | :interlaced_top_first | :interlaced_bottom_first | :mixed

  @type t :: %__MODULE__{
          width: pos_integer(),
          height: pos_integer(),
          framerate: {pos_integer(), pos_integer()} | nil,
          storage: DMABuf.Format.t(),
          colorimetry: Colorimetry.t(),
          pixel_aspect_ratio: {pos_integer(), pos_integer()},
          interlace_mode: interlace_mode(),
          alpha_mode: :opaque | :straight | :premultiplied
        }
end
