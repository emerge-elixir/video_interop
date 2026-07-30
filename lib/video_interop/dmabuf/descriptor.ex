defmodule VideoInterop.DMABuf.Descriptor do
  @moduledoc """
  DMA-BUF storage layout modeled after FFmpeg's `AVDRMFrameDescriptor`.

  Presentation geometry, colorimetry, synchronization and producer lifetime
  are deliberately kept outside this structure.
  """

  alias VideoInterop.DMABuf.{Layer, Object}

  @enforce_keys [:objects, :layers]
  defstruct version: 1, objects: [], layers: []

  @type t :: %__MODULE__{
          version: pos_integer(),
          objects: [Object.t()],
          layers: [Layer.t()]
        }
end
