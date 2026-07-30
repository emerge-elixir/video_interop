defmodule VideoInterop.Rect do
  @moduledoc "A visible rectangle inside a coded video image."

  @enforce_keys [:x, :y, :width, :height]
  defstruct @enforce_keys

  @type t :: %__MODULE__{
          x: non_neg_integer(),
          y: non_neg_integer(),
          width: pos_integer(),
          height: pos_integer()
        }
end
