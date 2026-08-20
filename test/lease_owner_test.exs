defmodule VideoInterop.LeaseOwnerTest do
  use ExUnit.Case, async: true

  alias VideoInterop.{Lease, LeaseOwner}

  test "issues a registered root lease from an isolated owner process" do
    test_pid = self()

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        max_active: 1,
        notify: self(),
        release: fn backend_token ->
          send(test_pid, {:backend_released, backend_token})
          :ok
        end
      )

    assert {:ok, lease} = LeaseOwner.issue(owner, :surface, metadata: %{sequence: 1})
    assert lease.owner == owner
    assert owner != self()
    assert is_reference(lease.token)
    assert is_reference(lease.holder)

    assert {:error, {:caller_owned, :capacity}} =
             LeaseOwner.issue(owner, :other_surface)

    refute_receive {:backend_released, :other_surface}

    assert :ok = Lease.release(lease)
    assert_receive {:backend_released, :surface}

    assert_receive {:video_interop_lease_released, ^owner, token, %{sequence: 1}, metrics}
    assert token == lease.token
    assert metrics.release_callback_ns >= 0

    stats = LeaseOwner.stats(owner)
    assert stats.active_leases == 0
    assert stats.active_holders == 0
    assert stats.release_callbacks == 1
    assert stats.release_failures == 0

    assert :ok = LeaseOwner.close(owner)
  end

  test "ordered cancellation removes a reservation whose reply times out" do
    test_pid = self()

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        release: fn token ->
          send(test_pid, {:backend_released, token})
          :ok
        end
      )

    :ok = :sys.suspend(owner)

    assert {:error, {:caller_owned, :timeout}} =
             LeaseOwner.issue(owner, :surface, timeout: 10)

    :ok = :sys.resume(owner)

    refute_receive {:backend_released, :surface}
    refute_receive {:video_interop_issued, _request_ref, _result}
    eventually(fn -> LeaseOwner.stats(owner).active_leases == 0 end)
    assert :ok = LeaseOwner.close(owner)
  end

  test "a timed-out reservation at capacity leaves its backend token caller-owned" do
    test_pid = self()

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        max_active: 1,
        release: fn token ->
          send(test_pid, {:backend_released, token})
          :ok
        end
      )

    assert {:ok, root} = LeaseOwner.issue(owner, :active)
    :ok = :sys.suspend(owner)

    assert {:error, {:caller_owned, :timeout}} =
             LeaseOwner.issue(owner, :timed_out, timeout: 10)

    :ok = :sys.resume(owner)

    refute_receive {:backend_released, :timed_out}
    assert :ok = Lease.release(root)
    assert_receive {:backend_released, :active}
    assert :ok = LeaseOwner.close(owner)
  end

  test "caller death releases a registered but unconfirmed issue" do
    test_pid = self()

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        release: fn token ->
          send(test_pid, {:backend_released, token})
          :ok
        end
      )

    caller =
      spawn(fn ->
        request_ref = Process.alias()
        send(owner, {:video_interop_reserve_issue, nil, self(), request_ref})

        receive do
          {:video_interop_issue_reserved, ^request_ref, {:ok, reservation}} ->
            send(
              owner,
              {:video_interop_commit_issue, reservation, :surface, self(), request_ref}
            )
        end

        receive do
          {:video_interop_issued, ^request_ref, {:ok, _lease}} ->
            send(test_pid, {:issue_sent, self()})
            Process.sleep(:infinity)
        end
      end)

    assert_receive {:issue_sent, ^caller}
    eventually(fn -> LeaseOwner.stats(owner).active_leases == 1 end)
    Process.exit(caller, :kill)

    assert_receive {:backend_released, :surface}
    eventually(fn -> LeaseOwner.stats(owner).active_leases == 0 end)
    assert :ok = LeaseOwner.close(owner)
  end

  test "releases the backend only after every fan-out holder retires" do
    test_pid = self()

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        release: fn token ->
          send(test_pid, {:backend_released, token})
          :ok
        end
      )

    assert {:ok, root} = LeaseOwner.issue(owner, :frame)
    assert {:ok, child} = Lease.retain(root)
    assert child.holder != root.holder

    assert :ok = Lease.release(root)
    refute_receive {:backend_released, :frame}, 20
    assert LeaseOwner.stats(owner).active_holders == 1

    assert :ok = Lease.release(child)
    assert_receive {:backend_released, :frame}

    assert :ok = Lease.release(child)
    assert LeaseOwner.stats(owner).duplicate_releases == 1
    assert :ok = LeaseOwner.close(owner)
  end

  test "ordered cancellation removes a retain that times out" do
    test_pid = self()

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        release: fn token ->
          send(test_pid, {:backend_released, token})
          :ok
        end
      )

    assert {:ok, root} = LeaseOwner.issue(owner, :frame)
    :ok = :sys.suspend(owner)
    assert Lease.retain(root, 10) == {:error, :timeout}
    :ok = :sys.resume(owner)

    eventually(fn -> LeaseOwner.stats(owner).retain_cancellations == 1 end)
    assert LeaseOwner.stats(owner).active_holders == 1
    refute_receive {:video_interop_retained, _request_ref, _result}

    assert :ok = Lease.release(root)
    assert_receive {:backend_released, :frame}
    assert :ok = LeaseOwner.close(owner)
  end

  test "caller death cancels an unconfirmed retain" do
    test_pid = self()

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        release: fn token ->
          send(test_pid, {:backend_released, token})
          :ok
        end
      )

    assert {:ok, root} = LeaseOwner.issue(owner, :frame)
    :ok = :sys.suspend(owner)

    retainer = spawn(fn -> Lease.retain(root, :infinity) end)
    eventually(fn -> elem(Process.info(owner, :message_queue_len), 1) >= 1 end)
    Process.exit(retainer, :kill)
    :ok = :sys.resume(owner)

    assert :ok = Lease.release(root)
    assert_receive {:backend_released, :frame}
    eventually(fn -> LeaseOwner.stats(owner).active_leases == 0 end)
    assert :ok = LeaseOwner.close(owner)
  end

  test "a blocked release callback does not block the owner mailbox" do
    test_pid = self()

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        release: fn token ->
          send(test_pid, {:release_callback_started, self(), token})

          receive do
            :complete_release -> :ok
          end
        end
      )

    assert {:ok, lease} = LeaseOwner.issue(owner, :blocked_release)
    assert :ok = Lease.release(lease)
    assert_receive {:release_callback_started, executor, :blocked_release}

    started = System.monotonic_time(:millisecond)
    stats = LeaseOwner.stats(owner, 100)
    assert stats.active_leases == 1
    assert stats.release_executor_active_age_ns >= 0
    assert System.monotonic_time(:millisecond) - started < 100

    send(executor, :complete_release)
    eventually(fn -> LeaseOwner.stats(owner).active_leases == 0 end)
    assert :ok = LeaseOwner.close(owner)
  end

  test "failed committed issues remain inside finite capacity" do
    test_pid = self()
    {:ok, attempts} = Agent.start_link(fn -> 0 end)

    factory = fn _owner, _token, _holder -> {:error, :injected_guard_failure} end

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        max_active: 1,
        abandonment_guard_factory: factory,
        release: fn token ->
          attempt = Agent.get_and_update(attempts, &{&1 + 1, &1 + 1})
          send(test_pid, {:bounded_release, token, attempt})
          if attempt == 1, do: {:error, :busy}, else: :ok
        end
      )

    assert {:error,
            {:transferred,
             {{:abandonment_guard_factory_failed, :injected_guard_failure},
              {:release_failed, public_token, :busy}}}} =
             LeaseOwner.issue(owner, :first)

    assert_receive {:bounded_release, :first, 1}
    assert LeaseOwner.stats(owner).active_leases == 1
    assert {:error, {:caller_owned, :capacity}} = LeaseOwner.issue(owner, :second)
    refute_receive {:bounded_release, :second, _attempt}

    assert :ok = LeaseOwner.retry(owner, public_token)
    assert_receive {:bounded_release, :first, 2}
    assert :ok = LeaseOwner.close(owner)
  end

  test "release executor crashes are visible and retryable" do
    test_pid = self()
    {:ok, attempts} = Agent.start_link(fn -> 0 end)

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        release_retry: {:exponential, initial_ms: 1, max_ms: 1, max_attempts: :infinity},
        release: fn token ->
          attempt = Agent.get_and_update(attempts, &{&1 + 1, &1 + 1})
          send(test_pid, {:executor_attempt, token, attempt})
          if attempt == 1, do: Process.exit(self(), :kill), else: :ok
        end
      )

    assert {:ok, lease} = LeaseOwner.issue(owner, :executor_crash)
    assert :ok = Lease.release(lease)
    assert_receive {:executor_attempt, :executor_crash, 1}
    assert_receive {:executor_attempt, :executor_crash, 2}
    eventually(fn -> LeaseOwner.stats(owner).active_leases == 0 end)
    assert LeaseOwner.stats(owner).release_executor_restarts == 1
    assert :ok = LeaseOwner.close(owner)
  end

  test "supervisor-owned start monitors and drains a distinct producer" do
    producer = spawn(fn -> Process.sleep(:infinity) end)

    {:ok, owner} =
      LeaseOwner.start_supervised(
        producer: producer,
        release: fn _token -> :ok end
      )

    monitor = Process.monitor(owner)
    assert {:ok, lease} = LeaseOwner.issue(owner, :supervised)
    Process.exit(producer, :kill)
    eventually(fn -> LeaseOwner.stats(owner).state == :draining end)
    assert :ok = Lease.release(lease)
    assert_receive {:DOWN, ^monitor, :process, ^owner, :normal}
  end

  test "release bypasses a blocked producer mailbox" do
    test_pid = self()

    producer =
      spawn(fn ->
        {:ok, owner} =
          LeaseOwner.start_link(
            producer: self(),
            release: fn token ->
              send(test_pid, {:backend_released, token})
              :ok
            end
          )

        send(test_pid, {:owner_started, self(), owner})

        receive do
          :stop -> :ok
        end
      end)

    assert_receive {:owner_started, ^producer, owner}
    assert {:ok, lease} = LeaseOwner.issue(owner, :surface)

    Enum.each(1..2_000, &send(producer, {:media_buffer, &1}))
    assert :ok = Lease.release(lease)
    assert_receive {:backend_released, :surface}, 500
    assert Process.alive?(producer)

    send(producer, :stop)
  end

  test "outlives its producer while draining an outstanding lease" do
    test_pid = self()

    producer =
      spawn(fn ->
        {:ok, owner} =
          LeaseOwner.start_link(
            producer: self(),
            notify: test_pid,
            release: fn token ->
              send(test_pid, {:backend_released, token})
              :ok
            end
          )

        send(test_pid, {:owner_started, self(), owner})

        receive do
          :stop -> :ok
        end
      end)

    assert_receive {:owner_started, ^producer, owner}
    monitor = Process.monitor(owner)
    assert {:ok, lease} = LeaseOwner.issue(owner, :surface)

    send(producer, :stop)
    eventually(fn -> LeaseOwner.stats(owner).state == :draining end)
    assert Process.alive?(owner)

    assert :ok = Lease.release(lease)
    assert_receive {:backend_released, :surface}
    assert_receive {:video_interop_lease_owner_drained, ^owner}
    assert_receive {:DOWN, ^monitor, :process, ^owner, :normal}
  end

  test "failed final release after producer shutdown remains retryable" do
    test_pid = self()
    {:ok, attempts} = Agent.start_link(fn -> 0 end)

    producer =
      spawn(fn ->
        {:ok, owner} =
          LeaseOwner.start_link(
            producer: self(),
            release: fn token ->
              attempt = Agent.get_and_update(attempts, &{&1 + 1, &1 + 1})
              send(test_pid, {:release_attempt, token, attempt})
              if attempt == 1, do: {:error, :busy}, else: :ok
            end
          )

        send(test_pid, {:owner_started, self(), owner})

        receive do
          :stop -> :ok
        end
      end)

    assert_receive {:owner_started, ^producer, owner}
    monitor = Process.monitor(owner)
    assert {:ok, lease} = LeaseOwner.issue(owner, :surface)

    send(producer, :stop)
    eventually(fn -> LeaseOwner.stats(owner).state == :draining end)
    assert :ok = Lease.release(lease)
    assert_receive {:release_attempt, :surface, 1}
    assert Process.alive?(owner)
    assert LeaseOwner.stats(owner).active_leases == 1

    assert :ok = LeaseOwner.retry(owner, lease.token)
    assert_receive {:release_attempt, :surface, 2}
    assert_receive {:DOWN, ^monitor, :process, ^owner, :normal}
  end

  test "unexpected owner failure is visible through the start_link relationship" do
    previous_trap_exit = Process.flag(:trap_exit, true)

    try do
      {:ok, owner} =
        LeaseOwner.start_link(producer: self(), release: fn _token -> :ok end)

      Process.exit(owner, :kill)
      assert_receive {:EXIT, ^owner, :killed}
    after
      Process.flag(:trap_exit, previous_trap_exit)
    end
  end

  test "token-bearing commits without reservations terminate instead of bypassing capacity" do
    previous_trap_exit = Process.flag(:trap_exit, true)

    try do
      {:ok, owner} =
        LeaseOwner.start_link(
          producer: self(),
          max_active: 1,
          release: fn token -> send(self(), {:unexpected_release, token}) end
        )

      assert {:ok, _lease} = LeaseOwner.issue(owner, :active)
      reservation = make_ref()

      send(
        owner,
        {:video_interop_commit_issue, reservation, :unreserved, self(), make_ref()}
      )

      assert_receive {:EXIT, ^owner, {:invalid_or_expired_issue_reservation, ^reservation}}
      refute_receive {:unexpected_release, :unreserved}
    after
      Process.flag(:trap_exit, previous_trap_exit)
    end
  end

  test "ignores malformed protocol messages without crashing" do
    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        max_active: 1,
        release: {__MODULE__, :release_for_test, [self()]}
      )

    send(owner, {:video_interop_retain, make_ref(), make_ref(), make_ref(), 123, make_ref()})
    send(owner, {:video_interop_cancel_retain, :not_a_reference, make_ref()})
    send(owner, {:video_interop_release, make_ref(), :not_a_reference})

    eventually(fn -> LeaseOwner.stats(owner).malformed_messages == 3 end)
    assert Process.alive?(owner)
    assert :ok = LeaseOwner.close(owner)
  end

  test "retains a failed release for explicit retry" do
    test_pid = self()
    {:ok, attempts} = Agent.start_link(fn -> 0 end)

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        notify_releases: true,
        release: fn token ->
          attempt = Agent.get_and_update(attempts, &{&1 + 1, &1 + 1})
          send(test_pid, {:release_attempt, token, attempt})
          if attempt == 1, do: {:error, :busy}, else: :ok
        end
      )

    assert {:ok, lease} = LeaseOwner.issue(owner, :surface)
    assert :ok = Lease.release(lease)
    assert_receive {:release_attempt, :surface, 1}

    assert_receive {:video_interop_lease_release_failed, ^owner, token, nil, :busy}
    assert token == lease.token
    assert LeaseOwner.stats(owner).active_leases == 1

    assert :ok = LeaseOwner.retry(owner, lease.token)
    assert_receive {:release_attempt, :surface, 2}
    assert_receive {:video_interop_lease_released, ^owner, ^token, nil, _metrics}
    assert LeaseOwner.stats(owner).active_leases == 0
    assert LeaseOwner.stats(owner).release_failures == 1

    assert :ok = LeaseOwner.close(owner)
  end

  test "already-dead owners leave issue tokens caller-owned without waiting" do
    owner = spawn(fn -> :ok end)
    monitor = Process.monitor(owner)
    assert_receive {:DOWN, ^monitor, :process, ^owner, _reason}

    started = System.monotonic_time(:millisecond)

    assert {:error, {:caller_owned, {:owner_down, _reason}}} =
             LeaseOwner.issue(owner, :surface, timeout: 1_000)

    assert System.monotonic_time(:millisecond) - started < 100
  end

  test "owner death after commit reports transferred ownership and clears monitor messages" do
    test_pid = self()

    owner =
      spawn(fn ->
        receive do
          {:video_interop_reserve_issue, nil, reply_to, request_ref} ->
            reservation = make_ref()
            send(request_ref, {:video_interop_issue_reserved, request_ref, {:ok, reservation}})

            receive do
              {:video_interop_commit_issue, ^reservation, :surface, ^reply_to, ^request_ref} ->
                send(test_pid, :issue_received)
                send(request_ref, {:video_interop_issued, request_ref, {:error, :shutting_down}})
                send(test_pid, {:reply_target, reply_to})
                exit(:shutdown)
            end
        end
      end)

    assert {:error, {:transferred, :shutting_down}} = LeaseOwner.issue(owner, :surface)
    assert_receive :issue_received
    assert_receive {:reply_target, pid} when pid == self()
    refute_receive {:DOWN, _monitor, :process, ^owner, _reason}
  end

  test "owner death while a reservation is in flight reports caller ownership" do
    previous_trap_exit = Process.flag(:trap_exit, true)

    try do
      {:ok, owner} =
        LeaseOwner.start_link(producer: self(), release: fn _token -> :ok end)

      :ok = :sys.suspend(owner)
      issuer = Task.async(fn -> LeaseOwner.issue(owner, :surface, timeout: 1_000) end)
      eventually(fn -> elem(Process.info(owner, :message_queue_len), 1) >= 1 end)
      Process.exit(owner, :kill)

      assert {:error, {:caller_owned, {:owner_down, :killed}}} = Task.await(issuer)
      assert_receive {:EXIT, ^owner, :killed}
    after
      Process.flag(:trap_exit, previous_trap_exit)
    end
  end

  test "drain rejects new issues and retains then completes after existing holders" do
    test_pid = self()

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        release: fn token ->
          send(test_pid, {:backend_released, token})
          :ok
        end
      )

    assert {:ok, root} = LeaseOwner.issue(owner, :root)
    drain_task = Task.async(fn -> LeaseOwner.drain(owner, 1_000) end)
    eventually(fn -> LeaseOwner.stats(owner).state == :draining end)

    assert {:error, :draining} = Lease.retain(root)

    assert {:error, {:caller_owned, :draining}} =
             LeaseOwner.issue(owner, :rejected)

    refute_receive {:backend_released, :rejected}
    assert :ok = Lease.release(root)
    assert_receive {:backend_released, :root}
    assert :ok = Task.await(drain_task)
  end

  test "drain waits for both root and retained child holders" do
    test_pid = self()

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        release: fn token ->
          send(test_pid, {:backend_released, token})
          :ok
        end
      )

    assert {:ok, root} = LeaseOwner.issue(owner, :frame)
    assert {:ok, child} = Lease.retain(root)
    waiter = Task.async(fn -> LeaseOwner.drain(owner, 1_000) end)
    eventually(fn -> LeaseOwner.stats(owner).state == :draining end)

    assert :ok = Lease.release(root)
    refute_receive {:backend_released, :frame}, 20
    assert LeaseOwner.stats(owner).active_holders == 1

    assert :ok = Lease.release(child)
    assert_receive {:backend_released, :frame}
    assert :ok = Task.await(waiter)
  end

  test "dead drain waiters are removed without changing drainage" do
    {:ok, owner} =
      LeaseOwner.start_link(producer: self(), release: fn _token -> :ok end)

    assert {:ok, root} = LeaseOwner.issue(owner, :frame)
    monitor = Process.monitor(owner)
    waiter = spawn(fn -> LeaseOwner.drain(owner, :infinity) end)
    eventually(fn -> LeaseOwner.stats(owner).drain_waiters == 1 end)

    Process.exit(waiter, :kill)
    eventually(fn -> LeaseOwner.stats(owner).drain_waiters == 0 end)
    assert LeaseOwner.stats(owner).state == :draining

    assert :ok = Lease.release(root)
    assert_receive {:DOWN, ^monitor, :process, ^owner, :normal}
  end

  test "owner mailbox order decides pending retain versus drain transition" do
    {:ok, first_owner} =
      LeaseOwner.start_link(producer: self(), release: fn _token -> :ok end)

    assert {:ok, first_root} = LeaseOwner.issue(first_owner, :first)
    :ok = :sys.suspend(first_owner)
    child_holder = make_ref()
    retain_ref = Process.alias()
    drain_ref = Process.alias()

    send(
      first_owner,
      {:video_interop_retain, first_root.token, first_root.holder, child_holder, self(),
       retain_ref}
    )

    send(first_owner, {:video_interop_drain, self(), drain_ref})
    :ok = :sys.resume(first_owner)

    assert_receive {:video_interop_retained, ^retain_ref, {:ok, nil}}
    send(first_owner, {:video_interop_confirm_retain, first_root.token, child_holder, retain_ref})
    Process.unalias(retain_ref)

    child = %{first_root | holder: child_holder}
    assert :ok = Lease.release(first_root)
    assert :ok = Lease.release(child)
    assert_receive {:video_interop_drained, ^drain_ref, :ok}
    Process.unalias(drain_ref)

    {:ok, second_owner} =
      LeaseOwner.start_link(producer: self(), release: fn _token -> :ok end)

    assert {:ok, second_root} = LeaseOwner.issue(second_owner, :second)
    :ok = :sys.suspend(second_owner)
    rejected_holder = make_ref()
    second_drain_ref = Process.alias()
    rejected_retain_ref = Process.alias()

    send(second_owner, {:video_interop_drain, self(), second_drain_ref})

    send(
      second_owner,
      {:video_interop_retain, second_root.token, second_root.holder, rejected_holder, self(),
       rejected_retain_ref}
    )

    :ok = :sys.resume(second_owner)
    assert_receive {:video_interop_retained, ^rejected_retain_ref, {:error, :draining}}
    Process.unalias(rejected_retain_ref)
    assert :ok = Lease.release(second_root)
    assert_receive {:video_interop_drained, ^second_drain_ref, :ok}
    Process.unalias(second_drain_ref)
  end

  test "drain timeout racing normal completion leaves a normally drained owner" do
    {:ok, owner} =
      LeaseOwner.start_link(producer: self(), release: fn _token -> :ok end)

    assert {:ok, root} = LeaseOwner.issue(owner, :frame)
    monitor = Process.monitor(owner)
    :ok = :sys.suspend(owner)
    test_pid = self()

    spawn(fn -> send(test_pid, {:drain_result, LeaseOwner.drain(owner, 10)}) end)
    eventually(fn -> elem(Process.info(owner, :message_queue_len), 1) >= 1 end)
    assert :ok = Lease.release(root)
    assert_receive {:drain_result, {:error, :timeout}}

    :ok = :sys.resume(owner)
    assert_receive {:DOWN, ^monitor, :process, ^owner, :normal}
  end

  test "a timed-out drain removes only its waiter" do
    {:ok, owner} =
      LeaseOwner.start_link(producer: self(), release: fn _token -> :ok end)

    assert {:ok, root} = LeaseOwner.issue(owner, :root)
    assert {:error, :timeout} = LeaseOwner.drain(owner, 10)
    assert Process.alive?(owner)

    second_waiter = Task.async(fn -> LeaseOwner.drain(owner, 1_000) end)
    eventually(fn -> LeaseOwner.stats(owner).drain_waiters == 1 end)
    assert :ok = Lease.release(root)
    assert :ok = Task.await(second_waiter)
  end

  test "drain identifies failed public token and retry completes it" do
    test_pid = self()
    {:ok, attempts} = Agent.start_link(fn -> 0 end)

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        release: fn backend_token ->
          attempt = Agent.get_and_update(attempts, &{&1 + 1, &1 + 1})
          send(test_pid, {:release_attempt, backend_token, attempt})
          if attempt == 1, do: {:error, :busy}, else: :ok
        end
      )

    assert {:ok, root} = LeaseOwner.issue(owner, :surface)
    waiter = Task.async(fn -> LeaseOwner.drain(owner, 1_000) end)
    eventually(fn -> LeaseOwner.stats(owner).state == :draining end)
    assert :ok = Lease.release(root)
    assert_receive {:release_attempt, :surface, 1}

    assert {:error, {:release_failed, token, :busy}} = Task.await(waiter)
    assert token == root.token
    assert :ok = LeaseOwner.retry(owner, token)
    assert_receive {:release_attempt, :surface, 2}
  end

  test "multiple drain waiters complete from the same final release" do
    {:ok, owner} =
      LeaseOwner.start_link(producer: self(), release: fn _token -> :ok end)

    assert {:ok, root} = LeaseOwner.issue(owner, :root)
    first = Task.async(fn -> LeaseOwner.drain(owner, 1_000) end)
    second = Task.async(fn -> LeaseOwner.drain(owner, 1_000) end)
    eventually(fn -> LeaseOwner.stats(owner).drain_waiters == 2 end)

    assert :ok = Lease.release(root)
    assert :ok = Task.await(first)
    assert :ok = Task.await(second)
  end

  test "automatic exponential retry is single-flight and completes drainage" do
    test_pid = self()
    {:ok, attempts} = Agent.start_link(fn -> 0 end)

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        release_retry: {:exponential, initial_ms: 2, max_ms: 4, max_attempts: :infinity},
        release: fn backend_token ->
          attempt = Agent.get_and_update(attempts, &{&1 + 1, &1 + 1})
          send(test_pid, {:release_attempt, backend_token, attempt})
          if attempt < 3, do: {:error, :busy}, else: :ok
        end
      )

    assert {:ok, root} = LeaseOwner.issue(owner, :surface)
    assert :ok = Lease.release(root)
    assert_receive {:release_attempt, :surface, 1}
    assert_receive {:release_attempt, :surface, 2}
    assert_receive {:release_attempt, :surface, 3}

    eventually(fn -> LeaseOwner.stats(owner).active_leases == 0 end)
    assert LeaseOwner.stats(owner).release_retries == 2
    assert Agent.get(attempts, & &1) == 3
    assert :ok = LeaseOwner.close(owner)
  end

  test "automatic retry completes while owner is already draining" do
    test_pid = self()
    {:ok, attempts} = Agent.start_link(fn -> 0 end)

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        notify: self(),
        release_retry: {:exponential, initial_ms: 2, max_ms: 2, max_attempts: :infinity},
        release: fn backend_token ->
          attempt = Agent.get_and_update(attempts, &{&1 + 1, &1 + 1})
          send(test_pid, {:draining_release_attempt, backend_token, attempt})
          if attempt == 1, do: {:error, :busy}, else: :ok
        end
      )

    monitor = Process.monitor(owner)
    assert {:ok, root} = LeaseOwner.issue(owner, :surface)
    assert {:ok, :draining} = LeaseOwner.close(owner)
    assert :ok = Lease.release(root)
    assert_receive {:draining_release_attempt, :surface, 1}
    assert_receive {:draining_release_attempt, :surface, 2}
    assert_receive {:video_interop_lease_owner_drained, ^owner}
    assert_receive {:DOWN, ^monitor, :process, ^owner, :normal}
  end

  test "automatic retry exhaustion after producer death remains manually retryable" do
    test_pid = self()
    {:ok, attempts} = Agent.start_link(fn -> 0 end)

    producer =
      spawn(fn ->
        {:ok, owner} =
          LeaseOwner.start_link(
            producer: self(),
            notify: nil,
            release_retry: {:exponential, initial_ms: 2, max_ms: 2, max_attempts: 2},
            release: fn backend_token ->
              attempt = Agent.get_and_update(attempts, &{&1 + 1, &1 + 1})
              send(test_pid, {:exhausted_release_attempt, backend_token, attempt})
              if attempt < 3, do: {:error, :busy}, else: :ok
            end
          )

        send(test_pid, {:exhaustion_owner_started, self(), owner})

        receive do
          :stop -> :ok
        end
      end)

    assert_receive {:exhaustion_owner_started, ^producer, owner}
    monitor = Process.monitor(owner)
    assert {:ok, root} = LeaseOwner.issue(owner, :surface)
    send(producer, :stop)
    eventually(fn -> LeaseOwner.stats(owner).state == :draining end)

    assert :ok = Lease.release(root)
    assert_receive {:exhausted_release_attempt, :surface, 1}
    assert_receive {:exhausted_release_attempt, :surface, 2}
    Process.sleep(10)
    refute_receive {:exhausted_release_attempt, :surface, 3}
    assert Process.alive?(owner)
    assert LeaseOwner.stats(owner).active_leases == 1

    assert :ok = LeaseOwner.retry(owner, root.token)
    assert_receive {:exhausted_release_attempt, :surface, 3}
    assert_receive {:DOWN, ^monitor, :process, ^owner, :normal}
  end

  test "manual retry cancels a stale automatic retry timer" do
    test_pid = self()
    {:ok, attempts} = Agent.start_link(fn -> 0 end)

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        release_retry: {:exponential, initial_ms: 100, max_ms: 100, max_attempts: :infinity},
        release: fn backend_token ->
          attempt = Agent.get_and_update(attempts, &{&1 + 1, &1 + 1})
          send(test_pid, {:release_attempt, backend_token, attempt})
          if attempt == 1, do: {:error, :busy}, else: :ok
        end
      )

    assert {:ok, root} = LeaseOwner.issue(owner, :surface)
    assert :ok = Lease.release(root)
    assert_receive {:release_attempt, :surface, 1}
    assert :ok = LeaseOwner.retry(owner, root.token)
    assert_receive {:release_attempt, :surface, 2}
    Process.sleep(120)
    refute_receive {:release_attempt, :surface, 3}
    assert Agent.get(attempts, & &1) == 2
    assert :ok = LeaseOwner.close(owner)
  end

  test "retry normalizes dead-owner failures instead of exiting" do
    owner = spawn(fn -> :ok end)
    monitor = Process.monitor(owner)
    assert_receive {:DOWN, ^monitor, :process, ^owner, _down_reason}
    assert {:error, {:owner_down, _retry_reason}} = LeaseOwner.retry(owner, make_ref(), 10)
  end

  @doc false
  def release_for_test(token, notify_to) do
    send(notify_to, {:backend_released, token})
    :ok
  end

  defp eventually(assertion, attempts \\ 100)
  defp eventually(assertion, 0), do: assert(assertion.())

  defp eventually(assertion, attempts) do
    if assertion.() do
      :ok
    else
      Process.sleep(1)
      eventually(assertion, attempts - 1)
    end
  end
end
