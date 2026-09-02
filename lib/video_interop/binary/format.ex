defmodule VideoInterop.Binary.Format do
  @moduledoc """
  Pixel interpretation for BEAM-owned binary frame storage.

  Gray2 and BW1 rows are packed independently, most-significant group first.
  Gray2 stores levels `0..3` from black to white. BW1 requires an explicit
  `:one_is_black` or `:one_is_white` polarity.
  """

  @enforce_keys [:pixel_format]
  defstruct pixel_format: nil, bw1_polarity: nil

  @type pixel_format :: :rgba8888 | :rgb888 | :gray8 | :gray2 | :bw1
  @type t :: %__MODULE__{
          pixel_format: pixel_format(),
          bw1_polarity: :one_is_black | :one_is_white | nil
        }
end
