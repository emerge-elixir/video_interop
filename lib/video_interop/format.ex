defmodule VideoInterop.Format do
  @moduledoc """
  Framework-neutral video format associated with a sequence of frames.

  `framerate: nil` represents an unknown cadence. `storage` contains the binary
  or DMA-BUF format separately from the outer video interpretation fields.
  `acquire_sync` declares implicit synchronization, sync files, or a
  compatibility per-frame mix.
  """

  alias VideoInterop.{Binary, Colorimetry}
  alias VideoInterop.DMABuf

  @enforce_keys [:width, :height, :framerate, :storage]
  defstruct width: nil,
            height: nil,
            framerate: nil,
            storage: nil,
            acquire_sync: :per_frame,
            colorimetry: %Colorimetry{},
            pixel_aspect_ratio: {1, 1},
            interlace_mode: :progressive,
            alpha_mode: :opaque

  @type interlace_mode ::
          :progressive | :interlaced_top_first | :interlaced_bottom_first | :mixed
  @type acquire_sync :: :implicit | :sync_file | :per_frame

  @type t :: %__MODULE__{
          width: pos_integer(),
          height: pos_integer(),
          framerate: {pos_integer(), pos_integer()} | nil,
          storage: Binary.Format.t() | DMABuf.Format.t(),
          acquire_sync: acquire_sync(),
          colorimetry: Colorimetry.t(),
          pixel_aspect_ratio: {pos_integer(), pos_integer()},
          interlace_mode: interlace_mode(),
          alpha_mode: :opaque | :straight | :premultiplied
        }
end
