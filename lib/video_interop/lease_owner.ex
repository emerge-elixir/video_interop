defmodule VideoInterop.LeaseOwner do
  @moduledoc """
  Isolated owner process for producer-backed video interop leases.

  One owner should be started per producing element or native buffer pool. Its mailbox is reserved
  for lease lifecycle messages so media traffic in the producer element cannot delay buffer
  retirement.

  The process implementation is deliberately private to this module. Callers depend on the PID and
  the public API, not on GenServer semantics.
  """

  use GenServer

  alias VideoInterop.Lease

  @type release_callback :: (term() -> term()) | {module(), atom(), [term()]}
  @type option ::
          {:producer, pid()}
          | {:release, release_callback()}
          | {:max_active, pos_integer() | :infinity}
          | {:notify, pid() | nil}
          | {:notify_releases, boolean()}

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
          release_callback_total_ns: non_neg_integer(),
          release_callback_max_ns: non_neg_integer(),
          malformed_messages: non_neg_integer(),
          message_queue_len: non_neg_integer()
        }

  @doc """
  Starts a lease owner linked to the producing process.

  The owner traps the producer's exit and drains already-issued leases instead of releasing them
  early. An abnormal owner exit is propagated through the link to the producer. Release failures
  notify the producer by default so the public token can be retried. If no configured observer is
  alive after producer shutdown and every holder has retired, a failed final release terminates the
  owner abnormally so backend-token destructors can run instead of leaving it stuck draining. Pass
  `notify: nil` only when another durable observer exists. Successful-release notifications are
  explicitly supplied, or with `notify_releases: true`.
  """
  @spec start_link([option()]) :: GenServer.on_start()
  def start_link(opts) do
    producer = Keyword.get(opts, :producer, self())
    GenServer.start(__MODULE__, Keyword.put(opts, :producer, producer))
  end

  @doc """
  Transfers a private backend token to the owner and returns a confirmed public root lease.

  Once the issue message is sent, the owner releases the backend token on capacity/draining
  rejection, timeout, caller death, or final holder retirement. `:metadata` is returned in
  diagnostics notifications. `:timeout` defaults to 5 seconds.
  """
  @spec issue(pid(), term(), keyword()) :: {:ok, Lease.t()} | {:error, term()}
  def issue(owner, backend_token, opts \\ []) when is_pid(owner) and is_list(opts) do
    request_ref = Process.alias()
    timeout = Keyword.get(opts, :timeout, 5_000)

    send(
      owner,
      {:video_interop_issue, backend_token, Keyword.get(opts, :metadata), self(), request_ref}
    )

    receive do
      {:video_interop_issued, ^request_ref, {:ok, %Lease{} = lease}} ->
        send(owner, {:video_interop_confirm_issue, lease.token, request_ref})
        Process.unalias(request_ref)
        {:ok, lease}

      {:video_interop_issued, ^request_ref, {:error, reason}} ->
        Process.unalias(request_ref)
        {:error, reason}
    after
      timeout ->
        Process.unalias(request_ref)
        send(owner, {:video_interop_cancel_issue, request_ref})
        {:error, :timeout}
    end
  end

  @doc "Stops accepting new leases and drains outstanding holders."
  @spec close(pid(), timeout()) :: :ok | {:ok, :draining}
  def close(owner, timeout \\ 5_000) when is_pid(owner) do
    GenServer.call(owner, :close, timeout)
  end

  @doc "Retries a final backend release that previously failed."
  @spec retry(pid(), reference(), timeout()) :: :ok | {:error, term()}
  def retry(owner, token, timeout \\ 5_000) when is_pid(owner) and is_reference(token) do
    GenServer.call(owner, {:retry, token}, timeout)
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

    validate_options!(producer, release, max_active, notify, notify_releases)
    Process.link(producer)

    {:ok,
     %{
       producer: producer,
       notify: notify,
       notify_releases: notify_releases,
       release: release,
       max_active: max_active,
       mode: :open,
       leases: %{},
       pending_issues: %{},
       retain_monitors: %{},
       counters: %{
         retain_requests: 0,
         retain_cancellations: 0,
         duplicate_releases: 0,
         release_callbacks: 0,
         release_failures: 0,
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
      notify_drained(state)
      {:stop, :normal, :ok, state}
    else
      {:reply, {:ok, :draining}, state}
    end
  end

  def handle_call({:retry, token}, _from, state) do
    case Map.fetch(state.leases, token) do
      {:ok, %{status: {:release_failed, _reason}} = entry} ->
        {reply, state} = release_entry(token, entry, state)
        stop_or_reply_after_release(reply, state)

      {:ok, _entry} ->
        {:reply, {:error, :not_release_failed}, state}

      :error ->
        {:reply, {:error, :unknown_lease}, state}
    end
  end

  def handle_call(:stats, _from, state) do
    {:reply, stats_snapshot(state), state}
  end

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
        now_ns = System.monotonic_time(:nanosecond)

        entry = %{
          backend_token: backend_token,
          holders: MapSet.new([holder]),
          issued_at_ns: now_ns,
          metadata: metadata,
          root_holder: holder,
          pending_issue: {monitor_ref, request_ref},
          pending_retains: %{},
          status: :active
        }

        lease = %Lease{owner: self(), token: token, holder: holder}

        state =
          state
          |> put_in([:leases, token], entry)
          |> put_in([:pending_issues, request_ref], token)
          |> put_in([:retain_monitors, monitor_ref], {:issue, token, request_ref})

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
              |> put_in([:retain_monitors, monitor_ref], {:retain, token, child_holder})

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
              |> update_in([:retain_monitors], &Map.delete(&1, monitor_ref))

            {:noreply, state}

          _other ->
            {:noreply, state}
        end

      _other ->
        {:noreply, state}
    end
  end

  def handle_info({:DOWN, monitor_ref, :process, _pid, _reason}, state) do
    case Map.pop(state.retain_monitors, monitor_ref) do
      {{:retain, token, child_holder}, retain_monitors} ->
        state = %{state | retain_monitors: retain_monitors}

        case Map.fetch(state.leases, token) do
          {:ok, %{status: :active} = entry} ->
            entry = %{entry | pending_retains: Map.delete(entry.pending_retains, child_holder)}
            remove_holder(token, child_holder, entry, state)

          _other ->
            {:noreply, state}
        end

      {{:issue, token, _request_ref}, retain_monitors} ->
        state = %{state | retain_monitors: retain_monitors}
        cancel_pending_issue(token, state)

      {nil, _retain_monitors} ->
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

  def handle_info({:EXIT, producer, _reason}, %{producer: producer} = state) do
    state = %{state | mode: :draining, producer: nil}

    cond do
      map_size(state.leases) == 0 ->
        notify_drained(state)
        {:stop, :normal, state}

      reason = unobserved_drained_failure(state) ->
        {:stop, reason, state}

      true ->
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
             :video_interop_release
           ] ->
        {:noreply, update_counter(state, :malformed_messages, 1)}

      _other ->
        {:noreply, state}
    end
  end

  def handle_info(_message, state), do: {:noreply, state}

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
      status: :active
    }

    {release_result, state} = release_entry(token, entry, state)

    reply =
      case release_result do
        :ok -> {:error, reason}
        {:error, release_reason} -> {:error, {reason, {:release_failed, token, release_reason}}}
      end

    reply_issue(reply_to, request_ref, reply)
    stop_or_continue_after_release(release_result, state)
  end

  defp cancel_pending_issue(token, state) do
    case Map.fetch(state.leases, token) do
      {:ok, %{status: :active, pending_issue: {_monitor_ref, _request_ref}} = entry} ->
        {entry, state} = clear_pending_issue(entry, state)
        entry = %{entry | holders: MapSet.new()}
        {result, state} = release_entry(token, entry, state)
        stop_or_continue_after_release(result, state)

      _other ->
        {:noreply, state}
    end
  end

  defp clear_pending_issue(%{pending_issue: nil} = entry, state), do: {entry, state}

  defp clear_pending_issue(
         %{pending_issue: {monitor_ref, request_ref}} = entry,
         state
       ) do
    Process.demonitor(monitor_ref, [:flush])

    state = %{
      state
      | pending_issues: Map.delete(state.pending_issues, request_ref),
        retain_monitors: Map.delete(state.retain_monitors, monitor_ref)
    }

    {%{entry | pending_issue: nil}, state}
  end

  defp clear_pending_retain(holder, entry, state) do
    case Map.pop(entry.pending_retains, holder) do
      {{monitor_ref, _request_ref}, pending_retains} ->
        Process.demonitor(monitor_ref, [:flush])

        entry = %{entry | pending_retains: pending_retains}
        state = update_in(state.retain_monitors, &Map.delete(&1, monitor_ref))
        {entry, state}

      {nil, _pending_retains} ->
        {entry, state}
    end
  end

  defp remove_holder(token, holder, entry, state) do
    if MapSet.member?(entry.holders, holder) do
      holders = MapSet.delete(entry.holders, holder)

      if MapSet.size(holders) == 0 do
        {result, state} = release_entry(token, %{entry | holders: holders}, state)
        stop_or_continue_after_release(result, state)
      else
        entry = %{entry | holders: holders}
        {:noreply, put_in(state.leases[token], entry)}
      end
    else
      {:noreply, update_counter(state, :duplicate_releases, 1)}
    end
  end

  defp release_entry(token, entry, state) do
    {entry, state} = clear_pending_issue(entry, state)
    {entry, state} = clear_all_pending_retains(entry, state)
    started_ns = System.monotonic_time(:nanosecond)
    callback_result = invoke_release(state.release, entry.backend_token)
    duration_ns = max(System.monotonic_time(:nanosecond) - started_ns, 0)

    state =
      state
      |> update_counter(:release_callbacks, 1)
      |> update_counter(:release_callback_total_ns, duration_ns)
      |> update_counter(:release_callback_max_ns, duration_ns, &max/2)

    case callback_result do
      {:ok, value} ->
        state = %{state | leases: Map.delete(state.leases, token)}

        if state.notify_releases do
          notify(
            state.notify,
            {:video_interop_lease_released, self(), token, entry.metadata,
             %{result: value, release_callback_ns: duration_ns}}
          )
        end

        {:ok, state}

      {:error, reason} ->
        failed_entry = %{entry | status: {:release_failed, reason}}

        state =
          state
          |> put_in([:leases, token], failed_entry)
          |> update_counter(:release_failures, 1)

        notify(
          state.notify,
          {:video_interop_lease_release_failed, self(), token, entry.metadata, reason}
        )

        {{:error, reason}, state}
    end
  end

  defp clear_all_pending_retains(entry, state) do
    state =
      Enum.reduce(entry.pending_retains, state, fn {_holder, {monitor_ref, _request_ref}},
                                                   state ->
        Process.demonitor(monitor_ref, [:flush])
        update_in(state.retain_monitors, &Map.delete(&1, monitor_ref))
      end)

    {%{entry | pending_retains: %{}}, state}
  end

  defp stop_or_continue_after_release(_result, state) do
    cond do
      state.mode == :draining and map_size(state.leases) == 0 ->
        notify_drained(state)
        {:stop, :normal, state}

      reason = unobserved_drained_failure(state) ->
        {:stop, reason, state}

      true ->
        {:noreply, state}
    end
  end

  defp stop_or_reply_after_release(reply, state) do
    cond do
      reply == :ok and state.mode == :draining and map_size(state.leases) == 0 ->
        notify_drained(state)
        {:stop, :normal, :ok, state}

      reason = unobserved_drained_failure(state) ->
        {:stop, reason, reply, state}

      true ->
        {:reply, reply, state}
    end
  end

  defp unobserved_drained_failure(%{mode: :draining} = state) do
    failures =
      Enum.flat_map(state.leases, fn
        {token, %{holders: holders, status: {:release_failed, reason}}} ->
          if MapSet.size(holders) == 0, do: [{token, reason}], else: []

        _entry ->
          []
      end)

    all_holders_retired? =
      Enum.all?(state.leases, fn {_token, entry} -> MapSet.size(entry.holders) == 0 end)

    if failures != [] and all_holders_retired? and not retry_observer_alive?(state.notify) do
      {:video_interop_release_failed, failures}
    end
  end

  defp unobserved_drained_failure(_state), do: nil

  defp retry_observer_alive?(pid) when is_pid(pid) and node(pid) == node(),
    do: Process.alive?(pid)

  defp retry_observer_alive?(_pid), do: false

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
    message = {:video_interop_retained, request_ref, result}

    reply_via_alias(reply_to, request_ref, message)
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

  defp validate_options!(producer, release, max_active, notify, notify_releases) do
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
  end

  defp valid_mfa?({module, function, arguments}),
    do: is_atom(module) and is_atom(function) and is_list(arguments)

  defp valid_mfa?(_other), do: false
end
