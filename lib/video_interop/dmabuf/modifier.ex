defmodule VideoInterop.DMABuf.Modifier do
  @moduledoc """
  DRM format modifier helpers.

  `:implicit` means that no explicit modifier metadata was supplied and an
  importer must omit modifier attributes. Integer `0` is different: it is the
  explicit DRM linear modifier.
  """

  @max_u64 0xFFFF_FFFF_FFFF_FFFF

  @type t :: :implicit | 0..0xFFFF_FFFF_FFFF_FFFF

  @spec linear() :: 0
  def linear, do: 0

  @spec valid?(term()) :: boolean()
  def valid?(:implicit), do: true

  def valid?(modifier) when is_integer(modifier) and modifier >= 0 and modifier <= @max_u64,
    do: true

  def valid?(_modifier), do: false
end
