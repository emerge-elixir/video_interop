defmodule VideoInterop.DMABuf.Layer do
  @moduledoc """
  One AVDRM-style format layer and its planes.

  A layer describes storage for one frame; it is not a compositor or UI layer.
  """

  alias VideoInterop.DMABuf.{FourCC, Plane}

  @enforce_keys [:fourcc, :planes]
  defstruct @enforce_keys

  @type t :: %__MODULE__{
          fourcc: FourCC.t(),
          planes: [Plane.t()]
        }
end
