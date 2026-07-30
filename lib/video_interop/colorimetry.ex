defmodule VideoInterop.Colorimetry do
  @moduledoc """
  Video color interpretation associated with a video stream.

  Values default to `:unspecified`; producers must not guess a matrix or range.
  Importers may reject unspecified values when correct conversion requires them.
  """

  defstruct primaries: :unspecified,
            transfer: :unspecified,
            matrix: :unspecified,
            range: :unspecified,
            chroma_location: :unspecified

  @type primaries ::
          :unspecified
          | :bt709
          | :bt470_m
          | :bt470_bg
          | :smpte170m
          | :smpte240m
          | :film
          | :bt2020
          | :smpte428
          | :smpte431
          | :smpte432
          | :ebu3213

  @type transfer ::
          :unspecified
          | :bt709
          | :gamma22
          | :gamma28
          | :smpte170m
          | :smpte240m
          | :linear
          | :log
          | :log_sqrt
          | :iec61966_2_4
          | :bt1361
          | :iec61966_2_1
          | :bt2020_10
          | :bt2020_12
          | :smpte2084
          | :smpte428
          | :arib_std_b67

  @type matrix ::
          :unspecified
          | :rgb
          | :bt709
          | :fcc
          | :bt470_bg
          | :smpte170m
          | :smpte240m
          | :ycgco
          | :bt2020_ncl
          | :bt2020_cl
          | :smpte2085
          | :chroma_derived_ncl
          | :chroma_derived_cl
          | :ictcp

  @type range :: :unspecified | :limited | :full
  @type chroma_location ::
          :unspecified | :left | :center | :top_left | :top | :bottom_left | :bottom

  @type t :: %__MODULE__{
          primaries: primaries(),
          transfer: transfer(),
          matrix: matrix(),
          range: range(),
          chroma_location: chroma_location()
        }
end
