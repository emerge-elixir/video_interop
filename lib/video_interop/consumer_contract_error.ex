defmodule VideoInterop.ConsumerContractError do
  @moduledoc """
  Raised when trusted consumer code violates the ownership receipt contract.

  `ownership: :unknown` means callers must not guess by releasing the frame,
  because the implementation may already have claimed it.
  """

  defexception [:operation, :result, :kind, :reason, :stacktrace, ownership: :unknown]

  @impl true
  def message(%__MODULE__{} = error) do
    details =
      if error.kind do
        " kind=#{inspect(error.kind)} reason=#{Exception.format_banner(error.kind, error.reason)}"
      else
        ""
      end

    "video consumer contract violation during #{error.operation}: " <>
      "ownership=#{inspect(error.ownership)} result=#{inspect(error.result)}#{details}"
  end
end
