defmodule VideoInterop.AbandonmentGuard do
  @moduledoc """
  Authenticated producer-native fallback attached to one lease holder.

  The BEAM represents both ordinary references and native resource references as
  references, so `is_reference/1` cannot prove that a value has a destructor.
  This envelope names the producer authority that registered the native
  resource. Strict transports call `valid?/1`; the authority must decode its own
  Rustler resource type and return exactly `true`.

  Authorities are trusted local code, not a sandbox boundary. The callback must
  be constant-time, nonblocking, lock-free, and fail closed during code upgrade.
  The envelope remains opaque to native consumers, which save the complete term
  until the corresponding claim retires.
  """

  @callback video_interop_abandonment_guard?(term()) :: boolean()

  @enforce_keys [:resource, :authority]
  defstruct [:resource, :authority]

  @type t :: %__MODULE__{resource: reference(), authority: module()}

  @doc "Builds an authority envelope around a producer-native resource."
  @spec new(reference(), module()) :: t()
  def new(resource, authority) when is_reference(resource) and is_atom(authority) do
    %__MODULE__{resource: resource, authority: authority}
  end

  @doc """
  Verifies the resource through its producer-specific authority.

  Missing modules/functions, exceptions, exits, throws, non-boolean replies,
  bare references, and malformed envelopes are rejected.
  """
  @spec valid?(term()) :: boolean()
  def valid?(%__MODULE__{resource: resource, authority: authority})
      when is_reference(resource) and is_atom(authority) do
    with {:module, ^authority} <- Code.ensure_loaded(authority),
         true <- function_exported?(authority, :video_interop_abandonment_guard?, 1) do
      try do
        authority.video_interop_abandonment_guard?(resource) === true
      rescue
        _error -> false
      catch
        _kind, _reason -> false
      end
    else
      _other -> false
    end
  end

  def valid?(_value), do: false
end
