defmodule VideoInterop.DMABuf.Format do
  @moduledoc "DMA-BUF pixel format and modifier negotiation metadata."

  alias VideoInterop.DMABuf.{FourCC, Modifier}

  @enforce_keys [:fourcc]
  defstruct fourcc: nil, modifier: :per_buffer

  @type modifier :: :per_buffer | Modifier.t()
  @type t :: %__MODULE__{fourcc: FourCC.t(), modifier: modifier()}
end
