defmodule VideoInterop do
  @moduledoc """
  Framework-neutral video frame interoperability contract.

  Version 0.1 describes Linux DMA-BUF storage, optional acquire sync-file
  fences, deterministic producer leases, and ownership-aware consumer streams.
  File descriptor integers in Elixir are borrowed and local to one OS process.
  Native consumers must validate and duplicate every descriptor before retaining
  it asynchronously.
  """

  alias VideoInterop.{Consumer, ConsumerContractError, ConsumerSession, Format, Frame, Lease}
  alias VideoInterop.DMABuf.Descriptor
  alias VideoInterop.Validator

  @spec retain(Frame.t(), timeout()) :: {:ok, Frame.t()} | {:error, term()}
  def retain(%Frame{lease: lease} = frame, timeout \\ 5_000) do
    case Lease.retain(lease, timeout) do
      {:ok, child_lease} -> {:ok, %{frame | lease: child_lease}}
      {:error, reason} -> {:error, reason}
    end
  end

  @spec release(Frame.t() | Lease.t()) :: :ok
  def release(%Frame{lease: lease}), do: Lease.release(lease)
  def release(%Lease{} = lease), do: Lease.release(lease)

  @doc """
  Opens an ownership-aware consumer session for a validated format.

  `owner:` defaults to the caller and must be a local PID. Consumer
  implementations use it to close the stream if its logical owner dies.
  """
  @spec open_consumer(Consumer.t(), Format.t(), keyword()) ::
          {:ok, ConsumerSession.t()} | {:error, term()}
  def open_consumer(consumer, %Format{} = format, opts \\ []) when is_list(opts) do
    owner = Keyword.get(opts, :owner, self())

    cond do
      not (is_pid(owner) and node(owner) == node()) ->
        {:error, :owner_must_be_a_local_pid}

      true ->
        case validate(format) do
          :ok ->
            if Consumer.impl_for(consumer) do
              open_validated_consumer(consumer, format, Keyword.put(opts, :owner, owner))
            else
              {:error, {:unsupported_consumer, consumer}}
            end

          {:error, _reason} = error ->
            error
        end
    end
  end

  @doc """
  Transfers a frame to an opened consumer session and consumes its holder.

  On every normal return the caller must not release the supplied frame. A known
  caller-owned rejection is released here. A transferred rejection remains the
  consumer's responsibility. Contract violations raise with ownership unknown
  rather than guessing and risking double release.
  """
  @spec consume(ConsumerSession.t(), Frame.t()) :: :ok | {:error, term()}
  def consume(session, %Frame{} = frame) do
    if ConsumerSession.impl_for(session) do
      case invoke_consumer!(:transfer, fn ->
             apply(ConsumerSession, :transfer, [session, frame])
           end) do
        {:ok, receipt} when receipt in [:transferred, :released] ->
          :ok

        {:error, {:caller_owned, reason}} ->
          :ok = release(frame)
          {:error, reason}

        {:error, {:transferred, reason}} ->
          {:error, reason}

        other ->
          raise ConsumerContractError, operation: :transfer, result: other
      end
    else
      :ok = release(frame)
      {:error, {:unsupported_consumer_session, session}}
    end
  end

  defp open_validated_consumer(consumer, format, opts) do
    case invoke_consumer!(:open, fn -> apply(Consumer, :open, [consumer, format, opts]) end) do
      {:ok, session} = opened ->
        if ConsumerSession.impl_for(session) do
          opened
        else
          raise ConsumerContractError,
            operation: :open,
            result: {:session_without_consumer_session_protocol, session}
        end

      {:error, _reason} = error ->
        error

      other ->
        raise ConsumerContractError, operation: :open, result: other
    end
  end

  @doc "Closes a consumer session with its implementation's idempotent close operation."
  @spec close_consumer(ConsumerSession.t()) :: :ok
  def close_consumer(session) do
    if ConsumerSession.impl_for(session) do
      case invoke_consumer!(:close, fn -> apply(ConsumerSession, :close, [session]) end) do
        :ok -> :ok
        other -> raise ConsumerContractError, operation: :close, result: other
      end
    else
      raise ConsumerContractError,
        operation: :close,
        result: {:unsupported_consumer_session, session}
    end
  end

  defp invoke_consumer!(operation, function) do
    try do
      function.()
    rescue
      error ->
        raise ConsumerContractError,
          operation: operation,
          result: {:exception, :error, error},
          kind: :error,
          reason: error,
          stacktrace: __STACKTRACE__
    catch
      kind, reason ->
        raise ConsumerContractError,
          operation: operation,
          result: {:exception, kind, reason},
          kind: kind,
          reason: reason,
          stacktrace: __STACKTRACE__
    end
  end

  @spec validate(Descriptor.t() | Frame.t() | Format.t()) ::
          :ok | {:error, Validator.reason()}
  def validate(%Descriptor{} = descriptor), do: Validator.validate_descriptor(descriptor)
  def validate(%Frame{} = frame), do: Validator.validate_frame(frame)
  def validate(%Format{} = format), do: Validator.validate_format(format)
  def validate(value), do: {:error, {:unsupported_value, value}}

  @spec validate(Frame.t(), Format.t()) :: :ok | {:error, Validator.reason()}
  def validate(%Frame{} = frame, %Format{} = format),
    do: Validator.validate_frame_against_format(frame, format)
end
