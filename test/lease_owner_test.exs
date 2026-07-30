defmodule VideoInterop.LeaseOwnerTest do
  use ExUnit.Case, async: true

  import ExUnit.CaptureLog

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
    assert {:error, :capacity} = LeaseOwner.issue(owner, :other_surface)
    assert_receive {:backend_released, :other_surface}

    assert :ok = Lease.release(lease)
    assert_receive {:backend_released, :surface}

    assert_receive {:video_interop_lease_released, ^owner, token, %{sequence: 1}, metrics}
    assert token == lease.token
    assert metrics.release_callback_ns >= 0

    stats = LeaseOwner.stats(owner)
    assert stats.active_leases == 0
    assert stats.active_holders == 0
    assert stats.release_callbacks == 2
    assert stats.release_failures == 0

    assert :ok = LeaseOwner.close(owner)
  end

  test "ordered cancellation releases an issue whose reply times out" do
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
    assert {:error, :timeout} = LeaseOwner.issue(owner, :surface, timeout: 10)
    :ok = :sys.resume(owner)

    assert_receive {:backend_released, :surface}
    refute_receive {:video_interop_issued, _request_ref, _result}
    eventually(fn -> LeaseOwner.stats(owner).active_leases == 0 end)
    assert :ok = LeaseOwner.close(owner)
  end

  test "a timed-out issue rejected at capacity still releases its backend token" do
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
    assert {:error, :timeout} = LeaseOwner.issue(owner, :timed_out, timeout: 10)
    :ok = :sys.resume(owner)

    assert_receive {:backend_released, :timed_out}
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
        send(owner, {:video_interop_issue, :surface, nil, self(), request_ref})
        send(test_pid, {:issue_sent, self()})
        Process.sleep(:infinity)
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

  test "failed final release after producer shutdown terminates without an observer" do
    test_pid = self()

    producer =
      spawn(fn ->
        {:ok, owner} =
          LeaseOwner.start_link(
            producer: self(),
            release: fn token ->
              send(test_pid, {:release_failed, token})
              {:error, :busy}
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

    capture_log(fn ->
      send(producer, :stop)
      eventually(fn -> LeaseOwner.stats(owner).state == :draining end)
      assert :ok = Lease.release(lease)
      assert_receive {:release_failed, :surface}

      assert_receive {:DOWN, ^monitor, :process, ^owner,
                      {:video_interop_release_failed, [{token, :busy}]}}

      assert token == lease.token
    end)
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
