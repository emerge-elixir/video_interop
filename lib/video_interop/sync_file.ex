defmodule VideoInterop.SyncFile do
  @moduledoc """
  Borrowed Linux sync-file acquire fence for a video frame.

  A native consumer must duplicate the fd before waiting asynchronously.
  Release-fence transport is intentionally outside the v0.1 contract.
  """

  @enforce_keys [:acquire_fence_fd]
  defstruct @enforce_keys

  @type t :: %__MODULE__{acquire_fence_fd: non_neg_integer()}
end
