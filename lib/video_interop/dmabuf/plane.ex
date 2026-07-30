defmodule VideoInterop.DMABuf.Plane do
  @moduledoc """
  A DRM format plane inside a DMA-BUF object.

  Object indices are zero-based. `offset` and `pitch` are measured in bytes.
  """

  @enforce_keys [:object_index, :offset, :pitch]
  defstruct @enforce_keys

  @type t :: %__MODULE__{
          object_index: non_neg_integer(),
          offset: non_neg_integer(),
          pitch: pos_integer()
        }
end
