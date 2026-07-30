defmodule VideoInterop.TestConsumer do
  @moduledoc false
  defstruct [:test_pid, :open_result]
end

defmodule VideoInterop.TestConsumerSession do
  @moduledoc false
  defstruct [:test_pid, :transfer, close_result: :ok]
end

defimpl VideoInterop.Consumer, for: VideoInterop.TestConsumer do
  def open(consumer, format, opts) do
    send(consumer.test_pid, {:consumer_opened, format, opts})

    case consumer.open_result do
      nil ->
        {:ok,
         %VideoInterop.TestConsumerSession{
           test_pid: consumer.test_pid,
           transfer: fn _frame -> {:ok, :transferred} end
         }}

      function when is_function(function, 0) ->
        function.()

      result ->
        result
    end
  end
end

defimpl VideoInterop.ConsumerSession, for: VideoInterop.TestConsumerSession do
  def transfer(session, frame) do
    send(session.test_pid, {:consumer_transfer, frame})
    session.transfer.(frame)
  end

  def close(session) do
    send(session.test_pid, :consumer_closed)

    case session.close_result do
      function when is_function(function, 0) -> function.()
      result -> result
    end
  end
end
