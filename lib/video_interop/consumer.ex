defprotocol VideoInterop.Consumer do
  @moduledoc """
  Opens an ownership-aware stream into a video frame consumer.

  Implementations validate the format, create a unique consumer-stream identity,
  and arrange for `owner:` death or native resource drop to close that stream.
  The returned value must implement `VideoInterop.ConsumerSession`.
  """

  alias VideoInterop.Format

  @fallback_to_any false

  @spec open(t(), Format.t(), keyword()) ::
          {:ok, VideoInterop.ConsumerSession.t()} | {:error, term()}
  def open(consumer, format, opts)
end
