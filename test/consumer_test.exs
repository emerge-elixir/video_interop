defmodule VideoInterop.ConsumerTest do
  use ExUnit.Case, async: true

  alias VideoInterop.{ConsumerContractError, Format, Frame, Lease, Rect}
  alias VideoInterop.DMABuf
  alias VideoInterop.DMABuf.{Descriptor, FourCC, Layer, Object, Plane}

  test "opens a validated consumer session with its local owner" do
    consumer = %VideoInterop.TestConsumer{test_pid: self()}

    assert {:ok, %VideoInterop.TestConsumerSession{} = session} =
             VideoInterop.open_consumer(consumer, format())

    assert_receive {:consumer_opened, opened_format, opts}
    assert opened_format == format()
    assert opts[:owner] == self()
    assert :ok = VideoInterop.close_consumer(session)
    assert_receive :consumer_closed
  end

  test "rejects invalid formats, owners, and unsupported consumers before open" do
    consumer = %VideoInterop.TestConsumer{test_pid: self()}
    invalid = %{format() | width: 0}

    assert {:error, _reason} = VideoInterop.open_consumer(consumer, invalid)
    refute_receive {:consumer_opened, _format, _opts}

    assert {:error, :owner_must_be_a_local_pid} =
             VideoInterop.open_consumer(consumer, format(), owner: :not_a_pid)

    assert {:error, {:unsupported_consumer, unsupported}} =
             VideoInterop.open_consumer(:unsupported, format())

    assert unsupported == :unsupported
  end

  test "rejects a consumer that returns a non-session value" do
    consumer = %VideoInterop.TestConsumer{test_pid: self(), open_result: {:ok, :not_a_session}}

    assert_raise ConsumerContractError, ~r/session_without_consumer_session_protocol/, fn ->
      VideoInterop.open_consumer(consumer, format())
    end
  end

  test "successful transferred receipt does not release in Elixir" do
    session = session(fn _frame -> {:ok, :transferred} end)
    frame = frame()

    assert :ok = VideoInterop.consume(session, frame)
    assert_receive {:consumer_transfer, ^frame}
    refute_receive {:video_interop_release, _, _}
  end

  test "successful released receipt is not released twice" do
    session =
      session(fn frame ->
        :ok = VideoInterop.release(frame)
        {:ok, :released}
      end)

    frame = frame()
    assert :ok = VideoInterop.consume(session, frame)
    assert_receive {:video_interop_release, token, holder}
    assert token == frame.lease.token
    assert holder == frame.lease.holder
    refute_receive {:video_interop_release, _, _}
  end

  test "caller-owned errors are released and transferred errors are not" do
    caller_owned = frame()

    assert {:error, :inactive} =
             VideoInterop.consume(
               session(fn _frame -> {:error, {:caller_owned, :inactive}} end),
               caller_owned
             )

    assert_receive {:video_interop_release, token, holder}
    assert token == caller_owned.lease.token
    assert holder == caller_owned.lease.holder

    transferred = frame()
    transferred_token = transferred.lease.token
    transferred_holder = transferred.lease.holder

    assert {:error, :late_failure} =
             VideoInterop.consume(
               session(fn _frame -> {:error, {:transferred, :late_failure}} end),
               transferred
             )

    refute_receive {:video_interop_release, ^transferred_token, ^transferred_holder}
  end

  test "unsupported sessions are known pre-transfer errors and release" do
    frame = frame()

    assert {:error, {:unsupported_consumer_session, :unsupported}} =
             VideoInterop.consume(:unsupported, frame)

    assert_receive {:video_interop_release, token, holder}
    assert token == frame.lease.token
    assert holder == frame.lease.holder
  end

  test "invalid receipts and transfer exceptions preserve unknown ownership" do
    invalid_frame = frame()
    invalid_token = invalid_frame.lease.token
    invalid_holder = invalid_frame.lease.holder

    invalid_error =
      assert_raise ConsumerContractError, ~r/ownership=:unknown/, fn ->
        VideoInterop.consume(session(fn _frame -> :invalid_receipt end), invalid_frame)
      end

    assert invalid_error.kind == nil
    refute_receive {:video_interop_release, ^invalid_token, ^invalid_holder}

    raised_frame = frame()
    raised_token = raised_frame.lease.token
    raised_holder = raised_frame.lease.holder

    raised_error =
      assert_raise ConsumerContractError, ~r/consumer failed/, fn ->
        VideoInterop.consume(session(fn _frame -> raise "consumer failed" end), raised_frame)
      end

    assert raised_error.kind == :error
    assert %RuntimeError{message: "consumer failed"} = raised_error.reason
    assert is_list(raised_error.stacktrace) and raised_error.stacktrace != []
    refute_receive {:video_interop_release, ^raised_token, ^raised_holder}

    thrown_frame = frame()
    thrown_token = thrown_frame.lease.token
    thrown_holder = thrown_frame.lease.holder

    thrown_error =
      assert_raise ConsumerContractError, ~r/thrown/, fn ->
        VideoInterop.consume(session(fn _frame -> throw(:thrown) end), thrown_frame)
      end

    assert thrown_error.kind == :throw
    assert thrown_error.reason == :thrown
    assert is_list(thrown_error.stacktrace) and thrown_error.stacktrace != []
    refute_receive {:video_interop_release, ^thrown_token, ^thrown_holder}
  end

  test "open and close implementation exceptions preserve original failure details" do
    open_error =
      assert_raise ConsumerContractError, ~r/open failed/, fn ->
        VideoInterop.open_consumer(
          %VideoInterop.TestConsumer{
            test_pid: self(),
            open_result: fn -> raise ArgumentError, "open failed" end
          },
          format()
        )
      end

    assert open_error.operation == :open
    assert open_error.kind == :error
    assert %ArgumentError{message: "open failed"} = open_error.reason
    assert is_list(open_error.stacktrace) and open_error.stacktrace != []

    close_error =
      assert_raise ConsumerContractError, ~r/close_failed/, fn ->
        VideoInterop.close_consumer(%VideoInterop.TestConsumerSession{
          test_pid: self(),
          transfer: fn _frame -> {:ok, :transferred} end,
          close_result: fn -> exit(:close_failed) end
        })
      end

    assert close_error.operation == :close
    assert close_error.kind == :exit
    assert close_error.reason == :close_failed
    assert is_list(close_error.stacktrace) and close_error.stacktrace != []
  end

  test "close contract rejects invalid results and unsupported sessions" do
    assert_raise ConsumerContractError, ~r/during close/, fn ->
      VideoInterop.close_consumer(%VideoInterop.TestConsumerSession{
        test_pid: self(),
        transfer: fn _frame -> {:ok, :transferred} end,
        close_result: {:error, :not_closed}
      })
    end

    assert_raise ConsumerContractError, ~r/unsupported_consumer_session/, fn ->
      VideoInterop.close_consumer(:unsupported)
    end
  end

  defp session(transfer) do
    %VideoInterop.TestConsumerSession{test_pid: self(), transfer: transfer}
  end

  defp format do
    %Format{
      width: 16,
      height: 16,
      framerate: {30, 1},
      storage: %DMABuf.Format{fourcc: FourCC.nv12()}
    }
  end

  defp frame do
    %Frame{
      coded_width: 16,
      coded_height: 16,
      visible_rect: %Rect{x: 0, y: 0, width: 16, height: 16},
      storage: %Descriptor{
        objects: [%Object{fd: 10, size: 384, modifier: :implicit}],
        layers: [
          %Layer{
            fourcc: FourCC.nv12(),
            planes: [
              %Plane{object_index: 0, offset: 0, pitch: 16},
              %Plane{object_index: 0, offset: 256, pitch: 16}
            ]
          }
        ]
      },
      lease: Lease.new(self(), make_ref())
    }
  end
end
