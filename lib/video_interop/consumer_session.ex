defprotocol VideoInterop.ConsumerSession do
  @moduledoc """
  An opened consumer stream with explicit frame ownership receipts.

  `transfer/2` must perform every fallible admission check before claiming the
  frame. After claim it must return a transferred receipt and retain a path that
  retires the holder exactly once. Implementations must not raise across an
  unknown claim point.

  `close/1` is idempotent and returns only after admission is closed and all
  pending/current claims are retired or scheduled for consumer-safe retirement.
  """

  alias VideoInterop.Frame

  @fallback_to_any false

  @type ownership_error :: {:caller_owned | :transferred, term()}
  @type transfer_result ::
          {:ok, :transferred | :released} | {:error, ownership_error()}

  @spec transfer(t(), Frame.t()) :: transfer_result()
  def transfer(session, frame)

  @spec close(t()) :: :ok
  def close(session)
end
