defmodule VideoInterop.Binary do
  @moduledoc """
  BEAM-owned binary video frame storage.

  The binary is immutable and owned by the frame term, so this storage does not
  require a `VideoInterop.Lease`. `planes` describes offsets and row strides
  inside `data`.
  """

  alias VideoInterop.Binary.Plane

  @enforce_keys [:data, :planes]
  defstruct [:data, :planes]

  @type t :: %__MODULE__{data: binary(), planes: [Plane.t()]}
end
