defmodule VideoInterop.Lease do
  @moduledoc """
  Per-consumer producer lifetime token for a borrowed video interop frame.

  A lease holder is unique. Before fan-out, a splitter must synchronously call
  `retain/2` for every additional branch and give each branch a different
  returned lease. The producer registers the new holder as pending before acknowledging
  the retain request. The caller then confirms receipt. It must process retain/confirm/cancel
  messages in mailbox order; a timeout sends
  `{:video_interop_cancel_retain, token, child_holder}` to remove a possibly late
  registration. Releases are idempotent per
  `{token, holder}` pair.

  Producers should issue managed leases through `VideoInterop.LeaseOwner`, whose isolated
  mailbox prevents media traffic in the producing element from delaying retirement. `new/2` is a
  low-level unmanaged constructor for implementations that provide equivalent registration,
  isolation, idempotency, and draining themselves.

  The opaque backend token should provide an owner-crash destructor fallback. A producer may also
  attach a unique `VideoInterop.AbandonmentGuard` authority envelope to each holder. Its verified
  native resource destructor is an eventual fallback for a holder-bearing BEAM term that disappears
  without an explicit release; normal release remains the deterministic primary path.
  """

  alias VideoInterop.AbandonmentGuard

  @retain_tag :video_interop_retain
  @retained_tag :video_interop_retained
  @confirm_retain_tag :video_interop_confirm_retain
  @cancel_retain_tag :video_interop_cancel_retain
  @release_tag :video_interop_release

  @enforce_keys [:owner, :token, :holder]
  defstruct owner: nil, token: nil, holder: nil, abandonment_guard: nil

  @type t :: %__MODULE__{
          owner: pid(),
          token: term(),
          holder: reference(),
          abandonment_guard: AbandonmentGuard.t() | nil
        }

  @doc """
  Creates an unmanaged root lease.

  Prefer `VideoInterop.LeaseOwner.issue/3`. This constructor does not register the holder or
  provide mailbox isolation, fan-out accounting, release callbacks, or shutdown draining.
  """
  @spec new(pid(), term()) :: t()
  def new(owner, token) when is_pid(owner),
    do: %__MODULE__{
      owner: owner,
      token: token,
      holder: make_ref(),
      abandonment_guard: nil
    }

  @doc """
  Synchronously obtains a unique child holder for an additional consumer.

  The owner receives:

      {:video_interop_retain, token, parent_holder, child_holder, reply_to, request_ref}

  `request_ref` is a process alias. The owner must register `child_holder` as pending, monitor
  `reply_to`, construct a fresh child guard, and send
  `{:video_interop_retained, request_ref, {:ok, child_guard}}` to the alias. Receipt is committed
  by `{:video_interop_confirm_retain, token, child_holder, request_ref}`; caller death or
  cancellation before that confirmation must remove the pending holder. The child lease replaces
  both the holder and guard; it never copies the parent's guard.
  """
  @spec retain(t(), timeout()) :: {:ok, t()} | {:error, :timeout | term()}
  def retain(%__MODULE__{} = lease, timeout \\ 5_000) do
    request_ref = Process.alias()
    child_holder = make_ref()

    send(
      lease.owner,
      {@retain_tag, lease.token, lease.holder, child_holder, self(), request_ref}
    )

    receive do
      {@retained_tag, ^request_ref, {:ok, child_guard}} ->
        if valid_child_guard?(lease.abandonment_guard, child_guard) do
          send(
            lease.owner,
            {@confirm_retain_tag, lease.token, child_holder, request_ref}
          )

          Process.unalias(request_ref)

          {:ok,
           %__MODULE__{
             owner: lease.owner,
             token: lease.token,
             holder: child_holder,
             abandonment_guard: child_guard
           }}
        else
          Process.unalias(request_ref)
          send(lease.owner, {@cancel_retain_tag, lease.token, child_holder})
          {:error, :invalid_abandonment_guard}
        end

      {@retained_tag, ^request_ref, {:error, reason}} ->
        Process.unalias(request_ref)
        {:error, reason}
    after
      timeout ->
        # Messages from this process to the owner are ordered. The owner therefore sees this
        # cancellation after the retain request and must remove child_holder whether or not it
        # already registered it. Removing the process alias also drops a late acknowledgement
        # instead of leaving it in the caller's mailbox.
        Process.unalias(request_ref)
        send(lease.owner, {@cancel_retain_tag, lease.token, child_holder})
        {:error, :timeout}
    end
  end

  defp valid_child_guard?(nil, nil), do: true
  defp valid_child_guard?(_parent_guard, child_guard), do: AbandonmentGuard.valid?(child_guard)

  @spec release(t()) :: :ok
  def release(%__MODULE__{owner: owner, token: token, holder: holder}) do
    send(owner, {@release_tag, token, holder})
    :ok
  end

  @spec retain_tag() :: :video_interop_retain
  def retain_tag, do: @retain_tag

  @spec retained_tag() :: :video_interop_retained
  def retained_tag, do: @retained_tag

  @spec confirm_retain_tag() :: :video_interop_confirm_retain
  def confirm_retain_tag, do: @confirm_retain_tag

  @spec cancel_retain_tag() :: :video_interop_cancel_retain
  def cancel_retain_tag, do: @cancel_retain_tag

  @spec release_tag() :: :video_interop_release
  def release_tag, do: @release_tag
end
