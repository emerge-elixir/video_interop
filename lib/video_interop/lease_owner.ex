defmodule VideoInterop.LeaseOwner do
  @moduledoc """
  Isolated owner process for producer-backed video interop leases.

  One owner should be started per producing element or native buffer pool. Its
  mailbox is reserved for lease lifecycle messages so media traffic in the
  producer cannot delay buffer retirement.

  Issuance has an explicit ownership boundary. The caller owns a backend token
  only when `issue/3` returns `{:error, {:caller_owned, reason}}`. Once the issue
  request is sent, every error is tagged `:transferred` and the owner (or the
  token's independent owner-crash destructor) is responsible for cleanup.

  Release callbacks used with automatic retry must be idempotent for one backend
  token. Retry is single-flight per public lease token. Failed entries remain
  alive and retryable after producer death or retry exhaustion; there is no
  implicit fatal policy based on observer liveness.
  """

  use GenServer

  alias VideoInterop.Lease

  @type release_callback :: (term() -> term()) | {module(), atom(), [term()]}
  @type retry_policy ::
          :manual
          | {:exponential,
             [
               initial_ms: pos_integer(),
               max_ms: pos_integer(),
               max_attempts: pos_integer() | :infinity
             ]}
  @type option ::
          {:producer, pid()}
          | {:release, release_callback()}
          | {:release_retry, retry_policy()}
          | {:max_active, pos_integer() | :infinity}
          | {:notify, pid() | nil}
          | {:notify_releases, boolean()}

  @type ownership_error :: {:caller_owned | :transferred, term()}

  @type stats :: %{
          state: :open | :draining,
          active_leases: non_neg_integer(),
          active_holders: non_neg_integer(),
          oldest_lease_age_ns: non_neg_integer() | nil,
          retain_requests: non_neg_integer(),
          retain_cancellations: non_neg_integer(),
          duplicate_releases: non_neg_integer(),
          release_callbacks: non_neg_integer(),
          release_failures: non_neg_integer(),
          release_retries: non_neg_integer(),
          release_callback_total_ns: non_neg_integer(),
          release_callback_max_ns: non_neg_integer(),
          malformed_messages: non_neg_integer(),
          drain_waiters: non_neg_integer(),
          message_queue_len: non_neg_integer()
        }

  @doc """
  Starts a lease owner linked to the producing process.

  The owner traps the producer's exit and drains issued leases instead of
  releasing them early. `release_retry` defaults to `:manual`. Automatic
  exponential retry requires an idempotent release callback.
  """
  @spec start_link([option()]) :: GenServer.on_start()
  def start_link(opts) do
    producer = Keyword.get(opts, :producer, self())
    GenServer.start(__MODULE__, Keyword.put(opts, :producer, producer))
  end

  @doc """
  Transfers a private backend token to the owner and returns a confirmed lease.

  The owner is monitored and checked before send. A `:caller_owned` error proves
  that no issue request was sent. The send is the transfer boundary; capacity,
  draining, timeout, release failure, and owner death after it are all
  `:transferred` errors.

  Because a local PID can die concurrently with send, backend tokens must also
  have an owner-crash/message-drop destructor fallback.
  """
  @spec issue(pid(), term(), keyword()) :: {:ok, Lease.t()} | {:error, ownership_error()}
  def issue(owner, backend_token, opts \\ []) when is_pid(owner) and is_list(opts) do
    if node(owner) == node() do
      do_issue(owner, backend_token, opts)
    else
      {:error, {:caller_owned, :owner_must_be_a_local_pid}}
    end
  end

  @doc "Stops accepting new issues and retains, then drains existing holders."
  @spec close(pid(), timeout()) :: :ok | {:ok, :draining}
  def close(owner, timeout \\ 5_000) when is_pid(owner) do
    GenServer.call(owner, :close, timeout)
  end

  @doc """
  Stops admission and waits for all holders and release callbacks.

  A timeout removes only this waiter and leaves the owner draining. A failed
  final callback returns its public token so `retry/3` can address it.
  """
  @spec drain(pid(), timeout()) ::
          :ok
          | {:error, :timeout | {:owner_down, term()} | {:release_failed, reference(), term()}}
  def drain(owner, timeout \\ 5_000)

  def drain(owner, timeout) when is_pid(owner) and node(owner) == node() do
    monitor_ref = Process.monitor(owner)

    if Process.alive?(owner) do
      request_ref = Process.alias()
      send(owner, {:video_interop_drain, self(), request_ref})
      await_drain(owner, request_ref, monitor_ref, timeout)
    else
      reason = owner_down_reason(monitor_ref, owner)
      Process.demonitor(monitor_ref, [:flush])
      {:error, {:owner_down, reason}}
    end
  end

  def drain(owner, _timeout) when is_pid(owner),
    do: {:error, {:owner_down, :owner_must_be_a_local_pid}}

  @doc "Retries a failed final backend release without exiting the caller."
  @spec retry(pid(), reference(), timeout()) :: :ok | {:error, term()}
  def retry(owner, token, timeout \\ 5_000) when is_pid(owner) and is_reference(token) do
    if node(owner) == node() do
      safe_call(owner, {:retry, token}, timeout)
    else
      {:error, {:owner_down, :owner_must_be_a_local_pid}}
    end
  end

  @doc "Returns lease counts, release timings, and mailbox depth."
  @spec stats(pid(), timeout()) :: stats()
  def stats(owner, timeout \\ 5_000) when is_pid(owner) do
    GenServer.call(owner, :stats, timeout)
  end

  @impl true
  def init(opts) do
    Process.flag(:trap_exit, true)

    producer = Keyword.fetch!(opts, :producer)
    release = Keyword.fetch!(opts, :release)
    max_active = Keyword.get(opts, :max_active, :infinity)
    notify = Keyword.get(opts, :notify, producer)
    notify_releases = Keyword.get(opts, :notify_releases, Keyword.has_key?(opts, :notify))
    release_retry = normalize_retry_policy(Keyword.get(opts, :release_retry, :manual))

    validate_options!(producer, release, max_active, notify, notify_releases, release_retry)
    Process.link(producer)

    {:ok,
     %{
       producer: producer,
       notify: notify,
       notify_releases: notify_releases,
       release: release,
       release_retry: release_retry,
       max_active: max_active,
       mode: :open,
       leases: %{},
       pending_issues: %{},
       protocol_monitors: %{},
       drain_waiters: %{},
       drain_monitors: %{},
       retry_timers: %{},
       counters: %{
         retain_requests: 0,
         retain_cancellations: 0,
         duplicate_releases: 0,
         release_callbacks: 0,
         release_failures: 0,
         release_retries: 0,
         release_callback_total_ns: 0,
         release_callback_max_ns: 0,
         malformed_messages: 0
       }
     }}
  end

  @impl true
  def handle_call(:close, _from, state) do
    state = %{state | mode: :draining}

    if map_size(state.leases) == 0 do
      state = complete_drain(state)
      {:stop, :normal, :ok, state}
    else
      {:reply, {:ok, :draining}, state}
    end
  end

  def handle_call({:retry, token}, _from, state) do
    state = cancel_retry_timer(token, state)

    case Map.fetch(state.leases, token) do
      {:ok, %{status: {:release_failed, _reason}} = entry} ->
        {reply, state} = release_entry(token, entry, state, retry?: true)
        stop_or_reply_after_release(reply, state)

      {:ok, _entry} ->
        {:reply, {:error, :not_release_failed}, state}

      :error ->
        {:reply, {:error, :unknown_lease}, state}
    end
  end

  def handle_call(:stats, _from, state), do: {:reply, stats_snapshot(state), state}

  @impl true
  def handle_info(
        {:video_interop_issue, backend_token, metadata, reply_to, request_ref},
        state
      )
      when is_pid(reply_to) and node(reply_to) == node() and is_reference(request_ref) do
    cond do
      state.mode == :draining ->
        reject_issue(:draining, backend_token, metadata, reply_to, request_ref, state)

      not capacity_available?(state) ->
        reject_issue(:capacity, backend_token, metadata, reply_to, request_ref, state)

      true ->
        token = make_ref()
        holder = make_ref()
        monitor_ref = Process.monitor(reply_to)

        entry = %{
          backend_token: backend_token,
          holders: MapSet.new([holder]),
          issued_at_ns: System.monotonic_time(:nanosecond),
          metadata: metadata,
          root_holder: holder,
          pending_issue: {monitor_ref, request_ref},
          pending_retains: %{},
          release_attempts: 0,
          status: :active
        }

        lease = %Lease{owner: self(), token: token, holder: holder}

        state =
          state
          |> put_in([:leases, token], entry)
          |> put_in([:pending_issues, request_ref], token)
          |> put_in([:protocol_monitors, monitor_ref], {:issue, token, request_ref})

        reply_issue(reply_to, request_ref, {:ok, lease})
        {:noreply, state}
    end
  end

  def handle_info({:video_interop_confirm_issue, token, request_ref}, state)
      when is_reference(token) and is_reference(request_ref) do
    case Map.fetch(state.leases, token) do
      {:ok, %{status: :active, pending_issue: {_monitor_ref, ^request_ref}} = entry} ->
        {entry, state} = clear_pending_issue(entry, state)
        {:noreply, put_in(state.leases[token], entry)}

      _other ->
        {:noreply, state}
    end
  end

  def handle_info({:video_interop_cancel_issue, request_ref}, state)
      when is_reference(request_ref) do
    case Map.fetch(state.pending_issues, request_ref) do
      {:ok, token} -> cancel_pending_issue(token, state)
      :error -> {:noreply, state}
    end
  end

  def handle_info(
        {:video_interop_retain, token, parent_holder, child_holder, reply_to, request_ref},
        state
      )
      when is_reference(token) and is_reference(parent_holder) and is_reference(child_holder) and
             is_pid(reply_to) and node(reply_to) == node() and is_reference(request_ref) do
    state = update_counter(state, :retain_requests, 1)

    cond do
      state.mode == :draining ->
        reply_retain(reply_to, request_ref, {:error, :draining})
        {:noreply, state}

      true ->
        retain_holder(token, parent_holder, child_holder, reply_to, request_ref, state)
    end
  end

  def handle_info(
        {:video_interop_confirm_retain, token, child_holder, request_ref},
        state
      )
      when is_reference(token) and is_reference(child_holder) and is_reference(request_ref) do
    case Map.fetch(state.leases, token) do
      {:ok, %{status: :active} = entry} ->
        case Map.fetch(entry.pending_retains, child_holder) do
          {:ok, {monitor_ref, ^request_ref}} ->
            Process.demonitor(monitor_ref, [:flush])

            state =
              state
              |> update_in([:leases, token, :pending_retains], &Map.delete(&1, child_holder))
              |> update_in([:protocol_monitors], &Map.delete(&1, monitor_ref))

            {:noreply, state}

          _other ->
            {:noreply, state}
        end

      _other ->
        {:noreply, state}
    end
  end

  def handle_info({:video_interop_cancel_retain, token, child_holder}, state)
      when is_reference(token) and is_reference(child_holder) do
    state = update_counter(state, :retain_cancellations, 1)

    case Map.fetch(state.leases, token) do
      {:ok, %{status: :active} = entry} when child_holder != entry.root_holder ->
        {entry, state} = clear_pending_retain(child_holder, entry, state)
        remove_holder(token, child_holder, entry, state)

      _other ->
        {:noreply, state}
    end
  end

  def handle_info({:video_interop_release, token, holder}, state)
      when is_reference(token) and is_reference(holder) do
    case Map.fetch(state.leases, token) do
      {:ok, %{status: :active} = entry} ->
        {entry, state} = clear_pending_retain(holder, entry, state)
        remove_holder(token, holder, entry, state)

      _other ->
        {:noreply, update_counter(state, :duplicate_releases, 1)}
    end
  end

  def handle_info({:video_interop_drain, reply_to, request_ref}, state)
      when is_pid(reply_to) and node(reply_to) == node() and is_reference(request_ref) do
    monitor_ref = Process.monitor(reply_to)

    state =
      state
      |> Map.put(:mode, :draining)
      |> put_in([:drain_waiters, request_ref], {reply_to, monitor_ref})
      |> put_in([:drain_monitors, monitor_ref], request_ref)

    case first_release_failure(state) do
      {token, reason} ->
        state = reply_drain_waiter(request_ref, {:error, {:release_failed, token, reason}}, state)
        {:noreply, state}

      nil when map_size(state.leases) == 0 ->
        state = complete_drain(state)
        {:stop, :normal, state}

      nil ->
        {:noreply, state}
    end
  end

  def handle_info({:video_interop_cancel_drain, request_ref}, state)
      when is_reference(request_ref) do
    {:noreply, remove_drain_waiter(request_ref, state)}
  end

  def handle_info({:video_interop_retry_release, token, generation}, state)
      when is_reference(token) and is_reference(generation) do
    case Map.fetch(state.retry_timers, token) do
      {:ok, %{generation: ^generation}} ->
        state = update_in(state.retry_timers, &Map.delete(&1, token))

        case Map.fetch(state.leases, token) do
          {:ok, %{status: {:release_failed, _reason}} = entry} ->
            {_reply, state} = release_entry(token, entry, state, retry?: true)
            stop_or_continue_after_release(state)

          _other ->
            {:noreply, state}
        end

      _other ->
        {:noreply, state}
    end
  end

  def handle_info({:DOWN, monitor_ref, :process, _pid, _reason}, state) do
    cond do
      Map.has_key?(state.protocol_monitors, monitor_ref) ->
        handle_protocol_down(monitor_ref, state)

      Map.has_key?(state.drain_monitors, monitor_ref) ->
        request_ref = Map.fetch!(state.drain_monitors, monitor_ref)
        {:noreply, remove_drain_waiter(request_ref, state)}

      true ->
        {:noreply, state}
    end
  end

  def handle_info({:EXIT, producer, _reason}, %{producer: producer} = state) do
    state = %{state | mode: :draining, producer: nil}

    if map_size(state.leases) == 0 do
      state = complete_drain(state)
      {:stop, :normal, state}
    else
      {:noreply, state}
    end
  end

  def handle_info(message, state) when is_tuple(message) and tuple_size(message) > 0 do
    case elem(message, 0) do
      tag
      when tag in [
             :video_interop_issue,
             :video_interop_confirm_issue,
             :video_interop_cancel_issue,
             :video_interop_retain,
             :video_interop_confirm_retain,
             :video_interop_cancel_retain,
             :video_interop_release,
             :video_interop_drain,
             :video_interop_cancel_drain,
             :video_interop_retry_release
           ] ->
        {:noreply, update_counter(state, :malformed_messages, 1)}

      _other ->
        {:noreply, state}
    end
  end

  def handle_info(_message, state), do: {:noreply, state}

  defp do_issue(owner, backend_token, opts) do
    timeout = Keyword.get(opts, :timeout, 5_000)
    monitor_ref = Process.monitor(owner)

    if Process.alive?(owner) do
      request_ref = Process.alias()

      send(
        owner,
        {:video_interop_issue, backend_token, Keyword.get(opts, :metadata), self(), request_ref}
      )

      await_issue(owner, request_ref, monitor_ref, timeout)
    else
      reason = owner_down_reason(monitor_ref, owner)
      Process.demonitor(monitor_ref, [:flush])
      {:error, {:caller_owned, {:owner_down, reason}}}
    end
  end

  defp await_issue(owner, request_ref, monitor_ref, timeout) do
    receive do
      {:video_interop_issued, ^request_ref, {:ok, %Lease{} = lease}} ->
        send(owner, {:video_interop_confirm_issue, lease.token, request_ref})
        cleanup_request(request_ref, monitor_ref)
        {:ok, lease}

      {:video_interop_issued, ^request_ref, {:error, reason}} ->
        cleanup_request(request_ref, monitor_ref)
        {:error, {:transferred, reason}}

      {:DOWN, ^monitor_ref, :process, ^owner, reason} ->
        Process.unalias(request_ref)
        {:error, {:transferred, {:owner_down, reason}}}
    after
      timeout ->
        Process.unalias(request_ref)
        send(owner, {:video_interop_cancel_issue, request_ref})
        Process.demonitor(monitor_ref, [:flush])
        {:error, {:transferred, :timeout}}
    end
  end

  defp await_drain(owner, request_ref, monitor_ref, timeout) do
    receive do
      {:video_interop_drained, ^request_ref, result} ->
        cleanup_request(request_ref, monitor_ref)
        result

      {:DOWN, ^monitor_ref, :process, ^owner, reason} ->
        Process.unalias(request_ref)
        {:error, {:owner_down, reason}}
    after
      timeout ->
        Process.unalias(request_ref)
        send(owner, {:video_interop_cancel_drain, request_ref})
        Process.demonitor(monitor_ref, [:flush])
        {:error, :timeout}
    end
  end

  defp cleanup_request(request_ref, monitor_ref) do
    Process.unalias(request_ref)
    Process.demonitor(monitor_ref, [:flush])
  end

  defp owner_down_reason(monitor_ref, owner) do
    receive do
      {:DOWN, ^monitor_ref, :process, ^owner, reason} -> reason
    after
      0 -> :noproc
    end
  end

  defp safe_call(owner, request, timeout) do
    try do
      GenServer.call(owner, request, timeout)
    catch
      :exit, {:timeout, _details} -> {:error, :timeout}
      :exit, {:noproc, _details} -> {:error, {:owner_down, :noproc}}
      :exit, {:normal, _details} -> {:error, {:owner_down, :normal}}
      :exit, reason -> {:error, {:owner_down, reason}}
    end
  end

  defp retain_holder(token, parent_holder, child_holder, reply_to, request_ref, state) do
    case Map.fetch(state.leases, token) do
      {:ok, %{status: :active} = entry} ->
        cond do
          not MapSet.member?(entry.holders, parent_holder) ->
            reply_retain(reply_to, request_ref, {:error, :unknown_parent_holder})
            {:noreply, state}

          MapSet.member?(entry.holders, child_holder) ->
            reply_retain(reply_to, request_ref, {:error, :duplicate_holder})
            {:noreply, state}

          true ->
            monitor_ref = Process.monitor(reply_to)

            entry = %{
              entry
              | holders: MapSet.put(entry.holders, child_holder),
                pending_retains:
                  Map.put(entry.pending_retains, child_holder, {monitor_ref, request_ref})
            }

            state =
              state
              |> put_in([:leases, token], entry)
              |> put_in([:protocol_monitors, monitor_ref], {:retain, token, child_holder})

            reply_retain(reply_to, request_ref, :ok)
            {:noreply, state}
        end

      {:ok, _entry} ->
        reply_retain(reply_to, request_ref, {:error, :lease_not_active})
        {:noreply, state}

      :error ->
        reply_retain(reply_to, request_ref, {:error, :unknown_lease})
        {:noreply, state}
    end
  end

  defp handle_protocol_down(monitor_ref, state) do
    case Map.pop(state.protocol_monitors, monitor_ref) do
      {{:retain, token, child_holder}, protocol_monitors} ->
        state = %{state | protocol_monitors: protocol_monitors}

        case Map.fetch(state.leases, token) do
          {:ok, %{status: :active} = entry} ->
            entry = %{entry | pending_retains: Map.delete(entry.pending_retains, child_holder)}
            remove_holder(token, child_holder, entry, state)

          _other ->
            {:noreply, state}
        end

      {{:issue, token, _request_ref}, protocol_monitors} ->
        state = %{state | protocol_monitors: protocol_monitors}
        cancel_pending_issue(token, state)

      {nil, _protocol_monitors} ->
        {:noreply, state}
    end
  end

  defp reject_issue(reason, backend_token, metadata, reply_to, request_ref, state) do
    token = make_ref()

    entry = %{
      backend_token: backend_token,
      holders: MapSet.new(),
      issued_at_ns: System.monotonic_time(:nanosecond),
      metadata: metadata,
      root_holder: nil,
      pending_issue: nil,
      pending_retains: %{},
      release_attempts: 0,
      status: :active
    }

    {release_result, state} = release_entry(token, entry, state)

    reply =
      case release_result do
        :ok -> {:error, reason}
        {:error, release_reason} -> {:error, {reason, {:release_failed, token, release_reason}}}
      end

    reply_issue(reply_to, request_ref, reply)
    stop_or_continue_after_release(state)
  end

  defp cancel_pending_issue(token, state) do
    case Map.fetch(state.leases, token) do
      {:ok, %{status: :active, pending_issue: {_monitor_ref, _request_ref}} = entry} ->
        {entry, state} = clear_pending_issue(entry, state)
        entry = %{entry | holders: MapSet.new()}
        {_result, state} = release_entry(token, entry, state)
        stop_or_continue_after_release(state)

      _other ->
        {:noreply, state}
    end
  end

  defp clear_pending_issue(%{pending_issue: nil} = entry, state), do: {entry, state}

  defp clear_pending_issue(%{pending_issue: {monitor_ref, request_ref}} = entry, state) do
    Process.demonitor(monitor_ref, [:flush])

    state = %{
      state
      | pending_issues: Map.delete(state.pending_issues, request_ref),
        protocol_monitors: Map.delete(state.protocol_monitors, monitor_ref)
    }

    {%{entry | pending_issue: nil}, state}
  end

  defp clear_pending_retain(holder, entry, state) do
    case Map.pop(entry.pending_retains, holder) do
      {{monitor_ref, _request_ref}, pending_retains} ->
        Process.demonitor(monitor_ref, [:flush])

        entry = %{entry | pending_retains: pending_retains}
        state = update_in(state.protocol_monitors, &Map.delete(&1, monitor_ref))
        {entry, state}

      {nil, _pending_retains} ->
        {entry, state}
    end
  end

  defp remove_holder(token, holder, entry, state) do
    if MapSet.member?(entry.holders, holder) do
      holders = MapSet.delete(entry.holders, holder)

      if MapSet.size(holders) == 0 do
        {_result, state} = release_entry(token, %{entry | holders: holders}, state)
        stop_or_continue_after_release(state)
      else
        {:noreply, put_in(state.leases[token], %{entry | holders: holders})}
      end
    else
      {:noreply, update_counter(state, :duplicate_releases, 1)}
    end
  end

  defp release_entry(token, entry, state, opts \\ []) do
    {entry, state} = clear_pending_issue(entry, state)
    {entry, state} = clear_all_pending_retains(entry, state)
    state = cancel_retry_timer(token, state)
    attempt = entry.release_attempts + 1
    started_ns = System.monotonic_time(:nanosecond)
    callback_result = invoke_release(state.release, entry.backend_token)
    duration_ns = max(System.monotonic_time(:nanosecond) - started_ns, 0)

    state =
      state
      |> update_counter(:release_callbacks, 1)
      |> maybe_count_retry(Keyword.get(opts, :retry?, false))
      |> update_counter(:release_callback_total_ns, duration_ns)
      |> update_counter(:release_callback_max_ns, duration_ns, &max/2)

    case callback_result do
      {:ok, value} ->
        state = %{state | leases: Map.delete(state.leases, token)}

        if state.notify_releases do
          notify(
            state.notify,
            {:video_interop_lease_released, self(), token, entry.metadata,
             %{result: value, release_callback_ns: duration_ns, attempt: attempt}}
          )
        end

        {:ok, state}

      {:error, reason} ->
        failed_entry = %{entry | release_attempts: attempt, status: {:release_failed, reason}}

        state =
          state
          |> put_in([:leases, token], failed_entry)
          |> update_counter(:release_failures, 1)

        notify(
          state.notify,
          {:video_interop_lease_release_failed, self(), token, entry.metadata, reason}
        )

        state = reply_all_drain_waiters({:error, {:release_failed, token, reason}}, state)
        state = schedule_retry(token, failed_entry, state)
        {{:error, reason}, state}
    end
  end

  defp clear_all_pending_retains(entry, state) do
    state =
      Enum.reduce(entry.pending_retains, state, fn {_holder, {monitor_ref, _request_ref}},
                                                   state ->
        Process.demonitor(monitor_ref, [:flush])
        update_in(state.protocol_monitors, &Map.delete(&1, monitor_ref))
      end)

    {%{entry | pending_retains: %{}}, state}
  end

  defp stop_or_continue_after_release(state) do
    if state.mode == :draining and map_size(state.leases) == 0 do
      state = complete_drain(state)
      {:stop, :normal, state}
    else
      {:noreply, state}
    end
  end

  defp stop_or_reply_after_release(reply, state) do
    if reply == :ok and state.mode == :draining and map_size(state.leases) == 0 do
      state = complete_drain(state)
      {:stop, :normal, :ok, state}
    else
      {:reply, reply, state}
    end
  end

  defp complete_drain(state) do
    state = reply_all_drain_waiters(:ok, state)
    notify_drained(state)
    state
  end

  defp first_release_failure(state) do
    Enum.find_value(state.leases, fn
      {token, %{holders: holders, status: {:release_failed, reason}}} ->
        if MapSet.size(holders) == 0, do: {token, reason}

      _entry ->
        nil
    end)
  end

  defp reply_all_drain_waiters(result, state) do
    Enum.reduce(Map.keys(state.drain_waiters), state, fn request_ref, state ->
      reply_drain_waiter(request_ref, result, state)
    end)
  end

  defp reply_drain_waiter(request_ref, result, state) do
    case state.drain_waiters[request_ref] do
      {reply_to, _monitor_ref} ->
        reply_via_alias(
          reply_to,
          request_ref,
          {:video_interop_drained, request_ref, result}
        )

        remove_drain_waiter(request_ref, state)

      nil ->
        state
    end
  end

  defp remove_drain_waiter(request_ref, state) do
    case Map.pop(state.drain_waiters, request_ref) do
      {{_reply_to, monitor_ref}, drain_waiters} ->
        Process.demonitor(monitor_ref, [:flush])

        %{
          state
          | drain_waiters: drain_waiters,
            drain_monitors: Map.delete(state.drain_monitors, monitor_ref)
        }

      {nil, _drain_waiters} ->
        state
    end
  end

  defp schedule_retry(_token, _entry, %{release_retry: :manual} = state), do: state

  defp schedule_retry(token, entry, %{release_retry: retry} = state) do
    if retry_available?(entry.release_attempts, retry.max_attempts) do
      delay = retry_delay(entry.release_attempts, retry)
      generation = make_ref()

      timer_ref =
        Process.send_after(self(), {:video_interop_retry_release, token, generation}, delay)

      put_in(state.retry_timers[token], %{
        timer_ref: timer_ref,
        generation: generation,
        attempt: entry.release_attempts + 1
      })
    else
      state
    end
  end

  defp cancel_retry_timer(token, state) do
    case Map.pop(state.retry_timers, token) do
      {%{timer_ref: timer_ref}, retry_timers} ->
        Process.cancel_timer(timer_ref)
        %{state | retry_timers: retry_timers}

      {nil, _retry_timers} ->
        state
    end
  end

  defp retry_available?(_attempts, :infinity), do: true
  defp retry_available?(attempts, max_attempts), do: attempts < max_attempts

  defp retry_delay(attempts, retry) do
    multiplier = Integer.pow(2, min(max(attempts - 1, 0), 30))
    min(retry.initial_ms * multiplier, retry.max_ms)
  end

  defp maybe_count_retry(state, true), do: update_counter(state, :release_retries, 1)
  defp maybe_count_retry(state, false), do: state

  defp invoke_release(callback, backend_token) do
    result =
      try do
        case callback do
          function when is_function(function, 1) -> function.(backend_token)
          {module, function, arguments} -> apply(module, function, [backend_token | arguments])
        end
      rescue
        error -> {:exception, error, __STACKTRACE__}
      catch
        kind, reason -> {kind, reason}
      end

    case result do
      :ok -> {:ok, :ok}
      {:ok, value} -> {:ok, value}
      {:error, reason} -> {:error, reason}
      {:exception, error, stacktrace} -> {:error, {:exception, error, stacktrace}}
      {kind, reason} when kind in [:exit, :throw] -> {:error, {kind, reason}}
      other -> {:error, {:invalid_release_result, other}}
    end
  end

  defp reply_issue(reply_to, request_ref, result) do
    reply_via_alias(reply_to, request_ref, {:video_interop_issued, request_ref, result})
  end

  defp reply_retain(reply_to, request_ref, result) do
    reply_via_alias(
      reply_to,
      request_ref,
      {:video_interop_retained, request_ref, result}
    )
  end

  defp reply_via_alias(reply_to, request_ref, message) do
    try do
      send(request_ref, message)
    rescue
      ArgumentError -> send(reply_to, message)
    end
  end

  defp capacity_available?(%{max_active: :infinity}), do: true
  defp capacity_available?(state), do: map_size(state.leases) < state.max_active

  defp stats_snapshot(state) do
    now_ns = System.monotonic_time(:nanosecond)

    oldest_lease_age_ns =
      state.leases
      |> Map.values()
      |> Enum.map(&max(now_ns - &1.issued_at_ns, 0))
      |> Enum.max(fn -> nil end)

    active_holders =
      state.leases
      |> Map.values()
      |> Enum.reduce(0, &(MapSet.size(&1.holders) + &2))

    queue_len =
      case Process.info(self(), :message_queue_len) do
        {:message_queue_len, value} -> value
        nil -> 0
      end

    state.counters
    |> Map.merge(%{
      state: state.mode,
      active_leases: map_size(state.leases),
      active_holders: active_holders,
      oldest_lease_age_ns: oldest_lease_age_ns,
      drain_waiters: map_size(state.drain_waiters),
      message_queue_len: queue_len
    })
  end

  defp update_counter(state, key, value, combine \\ &Kernel.+/2) do
    update_in(state.counters[key], &combine.(&1, value))
  end

  defp notify_drained(state) do
    notify(state.notify, {:video_interop_lease_owner_drained, self()})
  end

  defp notify(nil, _message), do: :ok
  defp notify(pid, message) when is_pid(pid), do: send(pid, message)

  defp normalize_retry_policy(:manual), do: :manual

  defp normalize_retry_policy({:exponential, opts}) when is_list(opts) do
    %{
      initial_ms: Keyword.get(opts, :initial_ms, 10),
      max_ms: Keyword.get(opts, :max_ms, 1_000),
      max_attempts: Keyword.get(opts, :max_attempts, :infinity)
    }
  end

  defp normalize_retry_policy(other), do: other

  defp validate_options!(producer, release, max_active, notify, notify_releases, retry) do
    unless is_pid(producer) and node(producer) == node() do
      raise ArgumentError, "producer must be a local PID"
    end

    unless is_function(release, 1) or valid_mfa?(release) do
      raise ArgumentError, "release must be a one-argument function or {module, function, args}"
    end

    unless max_active == :infinity or (is_integer(max_active) and max_active > 0) do
      raise ArgumentError, "max_active must be :infinity or a positive integer"
    end

    unless is_nil(notify) or is_pid(notify) do
      raise ArgumentError, "notify must be nil or a PID"
    end

    unless is_boolean(notify_releases) do
      raise ArgumentError, "notify_releases must be a boolean"
    end

    unless valid_retry_policy?(retry) do
      raise ArgumentError,
            "release_retry must be :manual or {:exponential, initial_ms: ..., max_ms: ..., max_attempts: ...}"
    end
  end

  defp valid_retry_policy?(:manual), do: true

  defp valid_retry_policy?(%{initial_ms: initial_ms, max_ms: max_ms, max_attempts: max_attempts}) do
    is_integer(initial_ms) and initial_ms > 0 and is_integer(max_ms) and max_ms >= initial_ms and
      (max_attempts == :infinity or (is_integer(max_attempts) and max_attempts > 0))
  end

  defp valid_retry_policy?(_other), do: false

  defp valid_mfa?({module, function, arguments}),
    do: is_atom(module) and is_atom(function) and is_list(arguments)

  defp valid_mfa?(_other), do: false
end
