defmodule VideoInterop.Binary.Plane do
  @moduledoc "A plane offset and row stride within `VideoInterop.Binary.data`."

  @enforce_keys [:offset, :stride]
  defstruct @enforce_keys

  @type t :: %__MODULE__{offset: non_neg_integer(), stride: pos_integer()}
end
