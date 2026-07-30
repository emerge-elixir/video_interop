defmodule VideoInterop.Frame do
  @moduledoc """
  One borrowed video frame and its producer lifetime lease.

  Timestamps belong to the transport using the frame. This structure describes
  storage, presentation geometry, acquire synchronization, and lifetime only.
  """

  alias VideoInterop.{Lease, Rect, SyncFile}
  alias VideoInterop.DMABuf.Descriptor

  @enforce_keys [:coded_width, :coded_height, :visible_rect, :storage, :lease]
  defstruct coded_width: nil,
            coded_height: nil,
            visible_rect: nil,
            storage: nil,
            acquire_sync: :implicit,
            lease: nil

  @type storage :: Descriptor.t()
  @type acquire_sync :: :implicit | SyncFile.t()

  @type t :: %__MODULE__{
          coded_width: pos_integer(),
          coded_height: pos_integer(),
          visible_rect: Rect.t(),
          storage: storage(),
          acquire_sync: acquire_sync(),
          lease: Lease.t()
        }
end
