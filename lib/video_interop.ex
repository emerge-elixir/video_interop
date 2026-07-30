defmodule VideoInterop do
  @moduledoc """
  Framework-neutral video frame interoperability contract.

  Version 0.1 describes Linux DMA-BUF storage, optional acquire sync-file
  fences, and deterministic producer leases. File descriptor integers in
  Elixir are borrowed and local to one OS process. Native consumers must
  validate and duplicate every descriptor before retaining it asynchronously.
  """

  alias VideoInterop.{Format, Frame, Lease, Validator}
  alias VideoInterop.DMABuf.Descriptor

  @spec release(Frame.t() | Lease.t()) :: :ok
  def release(%Frame{lease: lease}), do: Lease.release(lease)
  def release(%Lease{} = lease), do: Lease.release(lease)

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
