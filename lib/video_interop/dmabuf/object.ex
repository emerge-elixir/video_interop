defmodule VideoInterop.DMABuf.Object do
  @moduledoc """
  One DMA-BUF memory object referenced by a descriptor.

  `fd` is borrowed and remains valid only while the enclosing frame lease is
  retained. Native consumers must duplicate it before asynchronous use.
  """

  alias VideoInterop.DMABuf.Modifier

  @enforce_keys [:fd, :size, :modifier]
  defstruct @enforce_keys

  @type t :: %__MODULE__{
          fd: non_neg_integer(),
          size: pos_integer(),
          modifier: Modifier.t()
        }
end
