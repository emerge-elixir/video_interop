defmodule VideoInterop.AbandonmentGuardTest do
  use ExUnit.Case, async: false

  alias VideoInterop.{Frame, Lease, LeaseOwner, Rect, SchemaNative}
  alias VideoInterop.DMABuf.{Descriptor, FourCC, Layer, Object, Plane}

  test "killing the sole holder process eventually releases its guarded holder" do
    {owner, dispatcher} = start_guarded_owner()
    test_pid = self()

    holder_process =
      spawn(fn ->
        assert {:ok, lease} = LeaseOwner.issue(owner, :sole_holder)
        send(test_pid, {:issued, lease.token, lease.holder})
        Process.sleep(:infinity)
        _keep_complete_lease_live = lease
      end)

    assert_receive {:issued, token, holder}
    refute_receive {:backend_released, :sole_holder}, 50
    assert LeaseOwner.stats(owner).active_holders == 1

    holder_monitor = Process.monitor(holder_process)
    Process.exit(holder_process, :kill)
    assert_receive {:DOWN, ^holder_monitor, :process, ^holder_process, :killed}

    assert_receive {:backend_released, :sole_holder}, 1_000
    eventually(fn -> LeaseOwner.stats(owner).abandonments == 1 end)
    assert LeaseOwner.stats(owner).duplicate_releases == 0

    send(owner, {:video_interop_release, token, holder})
    eventually(fn -> LeaseOwner.stats(owner).late_releases_after_abandonment == 1 end)
    assert LeaseOwner.stats(owner).duplicate_releases == 0
    assert :ok = LeaseOwner.close(owner)
    assert is_reference(dispatcher)
  end

  test "killing a process with a guarded frame in its private queue releases it" do
    {owner, _dispatcher} = start_guarded_owner()
    test_pid = self()

    queue =
      spawn(fn ->
        assert {:ok, lease} = LeaseOwner.issue(owner, :queued_holder)
        send(self(), {:private_buffer, lease})
        send(test_pid, {:queued, lease.token})
        Process.sleep(:infinity)
      end)

    assert_receive {:queued, _token}
    refute_receive {:backend_released, :queued_holder}, 20
    Process.exit(queue, :kill)

    assert_receive {:backend_released, :queued_holder}, 1_000
    eventually(fn -> LeaseOwner.stats(owner).abandonments == 1 end)
    assert :ok = LeaseOwner.close(owner)
  end

  test "killing a retained child holder releases it without incidental owner GC" do
    {owner, _dispatcher} = start_guarded_owner()
    test_pid = self()

    assert {:ok, root} = LeaseOwner.issue(owner, :retained_queue_holder)

    queue =
      spawn(fn ->
        assert {:ok, child} = Lease.retain(root)
        send(self(), {:private_buffer, child})
        send(test_pid, {:retained_child_queued, child.holder})
        Process.sleep(:infinity)
      end)

    assert_receive {:retained_child_queued, child_holder}
    refute_receive {:backend_released, :retained_queue_holder}, 20

    queue_monitor = Process.monitor(queue)
    Process.exit(queue, :kill)
    assert_receive {:DOWN, ^queue_monitor, :process, ^queue, :killed}

    eventually(fn ->
      stats = LeaseOwner.stats(owner)
      stats.abandonments == 1 and stats.active_holders == 1
    end)

    send(owner, {:video_interop_release, root.token, child_holder})
    eventually(fn -> LeaseOwner.stats(owner).late_releases_after_abandonment == 1 end)

    assert :ok = Lease.release(root)
    assert_receive {:backend_released, :retained_queue_holder}, 1_000
    assert :ok = LeaseOwner.close(owner)
  end

  test "an unclaimed prepared frame leaves the original BEAM guard responsible" do
    {owner, dispatcher} = start_guarded_owner()
    assert {:ok, {fd, fd_resource}} = SchemaNative.open_test_fd()
    assert {:ok, lease} = LeaseOwner.issue(owner, :prepared_only)
    original_frame = frame(fd, lease)

    assert SchemaNative.prepare_and_drop_frame(original_frame, dispatcher) == {:ok, true}
    assert true = :erlang.garbage_collect(owner)
    refute_receive {:backend_released, :prepared_only}, 20
    assert LeaseOwner.stats(owner).active_holders == 1

    assert :ok = Lease.release(original_frame.lease)
    assert_receive {:backend_released, :prepared_only}
    assert is_reference(fd_resource)
    assert :ok = LeaseOwner.close(owner)
  end

  test "a claimed native frame retains the guard after the original BEAM process dies" do
    {owner, dispatcher} = start_guarded_owner()
    assert {:ok, {fd, fd_resource}} = SchemaNative.open_test_fd()
    test_pid = self()

    claimant =
      spawn(fn ->
        assert {:ok, lease} = LeaseOwner.issue(owner, :native_claim)
        frame = frame(fd, lease)
        assert {:ok, claim} = SchemaNative.claim_frame(frame, dispatcher)
        send(test_pid, {:claimed, claim})
        Process.sleep(:infinity)
      end)

    assert_receive {:claimed, claim}
    Process.exit(claimant, :kill)
    Process.sleep(20)

    assert LeaseOwner.stats(owner).active_holders == 1
    assert LeaseOwner.stats(owner).abandonments == 0
    refute_receive {:backend_released, :native_claim}, 20

    assert SchemaNative.retire_claim(claim) == {:ok, true}
    assert_receive {:backend_released, :native_claim}, 1_000
    eventually(fn -> LeaseOwner.stats(owner).active_holders == 0 end)
    assert LeaseOwner.stats(owner).duplicate_releases == 0
    assert is_reference(fd_resource)
    assert :ok = LeaseOwner.close(owner)
  end

  test "root and fan-out child guards are unique and both holders are required" do
    {owner, _dispatcher} = start_guarded_owner()

    assert {:ok, root} = LeaseOwner.issue(owner, :fanout)
    assert {:ok, first} = Lease.retain(root)
    assert {:ok, second} = Lease.retain(root)

    assert VideoInterop.AbandonmentGuard.valid?(root.abandonment_guard)
    assert VideoInterop.AbandonmentGuard.valid?(first.abandonment_guard)
    assert VideoInterop.AbandonmentGuard.valid?(second.abandonment_guard)
    refute root.abandonment_guard == first.abandonment_guard
    refute root.abandonment_guard == second.abandonment_guard
    refute first.abandonment_guard == second.abandonment_guard
    assert MapSet.size(MapSet.new([root.holder, first.holder, second.holder])) == 3

    :ok = Lease.release(root)
    :ok = Lease.release(first)
    refute_receive {:backend_released, :fanout}, 20
    :ok = Lease.release(second)
    assert_receive {:backend_released, :fanout}
    assert :ok = LeaseOwner.close(owner)
  end

  test "root guard factory failure publishes no holder and releases transferred backend" do
    test_pid = self()

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        abandonment_guard_factory: fn _owner, _token, _holder -> {:error, :unavailable} end,
        release: fn backend ->
          send(test_pid, {:backend_released, backend})
          :ok
        end
      )

    assert {:error, {:transferred, {:abandonment_guard_factory_failed, :unavailable}}} =
             LeaseOwner.issue(owner, :root_failure)

    assert_receive {:backend_released, :root_failure}
    assert LeaseOwner.stats(owner).active_holders == 0
    assert LeaseOwner.stats(owner).issued_leases == 0
    assert :ok = LeaseOwner.close(owner)
  end

  test "retain guard factory failure registers no child holder" do
    assert {:ok, {dispatcher, _probe}} = SchemaNative.start_dispatcher()
    on_exit(fn -> assert SchemaNative.shutdown_dispatcher(dispatcher) == {:ok, true} end)
    {:ok, calls} = Agent.start_link(fn -> 0 end)
    test_pid = self()

    factory = fn lease_owner, token, holder ->
      call = Agent.get_and_update(calls, &{&1 + 1, &1 + 1})

      if call == 1 do
        SchemaNative.new_abandonment_guard(dispatcher, lease_owner, token, holder)
      else
        {:error, :injected_child_failure}
      end
    end

    {:ok, owner} =
      LeaseOwner.start_link(
        producer: self(),
        abandonment_guard_factory: factory,
        release: fn backend ->
          send(test_pid, {:backend_released, backend})
          :ok
        end
      )

    assert {:ok, root} = LeaseOwner.issue(owner, :retain_failure)

    assert {:error, {:abandonment_guard_factory_failed, :injected_child_failure}} =
             Lease.retain(root)

    assert LeaseOwner.stats(owner).active_holders == 1
    assert LeaseOwner.stats(owner).issued_holders == 1
    :ok = Lease.release(root)
    assert_receive {:backend_released, :retain_failure}
    assert :ok = LeaseOwner.close(owner)
  end

  test "timed-out issue reservation never publishes a guard or transfers its token" do
    {owner, _dispatcher} = start_guarded_owner()
    :ok = :sys.suspend(owner)

    assert {:error, {:caller_owned, :timeout}} =
             LeaseOwner.issue(owner, :timed_out_guard, timeout: 10)

    :ok = :sys.resume(owner)
    refute_receive {:backend_released, :timed_out_guard}, 20
    eventually(fn -> LeaseOwner.stats(owner).active_holders == 0 end)
    assert LeaseOwner.stats(owner).duplicate_releases == 0
    assert :ok = LeaseOwner.close(owner)
  end

  test "timed-out retained guard replies race cancellation idempotently" do
    {owner, _dispatcher} = start_guarded_owner()
    assert {:ok, root} = LeaseOwner.issue(owner, :retain_timeout)
    :ok = :sys.suspend(owner)

    assert {:error, :timeout} = Lease.retain(root, 10)
    :ok = :sys.resume(owner)

    eventually(fn -> LeaseOwner.stats(owner).retain_cancellations == 1 end)
    assert true = :erlang.garbage_collect(owner)
    eventually(fn -> LeaseOwner.stats(owner).active_holders == 1 end)
    assert LeaseOwner.stats(owner).duplicate_releases == 0

    assert :ok = Lease.release(root)
    assert_receive {:backend_released, :retain_timeout}
    assert :ok = LeaseOwner.close(owner)
  end

  test "explicit then fallback and fallback then explicit are idempotent" do
    {owner, _dispatcher} = start_guarded_owner()
    test_pid = self()

    explicit_first =
      spawn(fn ->
        assert {:ok, lease} = LeaseOwner.issue(owner, :explicit_first)
        send(test_pid, {:explicit_lease, lease.token, lease.holder})

        receive do
          :release ->
            :ok = Lease.release(lease)
            send(test_pid, :explicit_sent)
            Process.sleep(:infinity)
        end
      end)

    assert_receive {:explicit_lease, _explicit_token, _explicit_holder}
    send(explicit_first, :release)
    assert_receive :explicit_sent
    assert_receive {:backend_released, :explicit_first}
    Process.exit(explicit_first, :kill)
    Process.sleep(20)
    assert LeaseOwner.stats(owner).duplicate_releases == 0
    assert LeaseOwner.stats(owner).abandonments == 0

    fallback_first =
      spawn(fn ->
        assert {:ok, lease} = LeaseOwner.issue(owner, :fallback_first)
        send(test_pid, {:fallback_ids, lease.token, lease.holder})
        Process.sleep(:infinity)
      end)

    assert_receive {:fallback_ids, token, holder}
    assert true = :erlang.garbage_collect(owner)
    fallback_monitor = Process.monitor(fallback_first)
    Process.exit(fallback_first, :kill)
    assert_receive {:DOWN, ^fallback_monitor, :process, ^fallback_first, :killed}
    assert_receive {:backend_released, :fallback_first}, 1_000
    send(owner, {:video_interop_release, token, holder})
    eventually(fn -> LeaseOwner.stats(owner).abandonments == 1 end)
    assert LeaseOwner.stats(owner).duplicate_releases == 0
    assert :ok = LeaseOwner.close(owner)
  end

  test "bounded tombstones classify known duplicates and report evicted releases honestly" do
    {owner, _dispatcher} = start_guarded_owner()

    leases =
      for backend <- 1..1_025 do
        assert {:ok, lease} = LeaseOwner.issue(owner, {:tombstone, backend})
        send(owner, {:video_interop_abandoned, lease.token, lease.holder})
        lease
      end

    eventually(fn -> LeaseOwner.stats(owner).abandonments == 1_025 end)
    stats = LeaseOwner.stats(owner)
    assert stats.release_tombstone_limit == 1_024
    assert stats.release_tombstones == stats.release_tombstone_limit
    assert stats.release_tombstone_evictions == 1
    assert stats.abandonment_tombstone_evictions == 1
    assert stats.duplicate_releases == 0
    assert stats.unclassified_releases == 0

    [evicted, known | _rest] = leases
    send(owner, {:video_interop_release, evicted.token, evicted.holder})
    eventually(fn -> LeaseOwner.stats(owner).unclassified_releases == 1 end)
    assert LeaseOwner.stats(owner).duplicate_releases == 0

    send(owner, {:video_interop_release, known.token, known.holder})
    eventually(fn -> LeaseOwner.stats(owner).late_releases_after_abandonment == 1 end)
    assert LeaseOwner.stats(owner).duplicate_releases == 0

    send(owner, {:video_interop_release, known.token, known.holder})
    eventually(fn -> LeaseOwner.stats(owner).duplicate_releases == 1 end)
    assert length(leases) == 1_025
    assert :ok = LeaseOwner.close(owner)
  end

  test "final immutable stats precede the compatible two-field drained notification" do
    {owner, _dispatcher} = start_guarded_owner(notify: self())

    assert {:ok, lease} = LeaseOwner.issue(owner, :snapshot)
    assert :ok = Lease.release(lease)
    assert_receive {:backend_released, :snapshot}
    assert :ok = LeaseOwner.close(owner)

    assert_receive {:video_interop_lease_owner_final_stats, ^owner, stats}
    assert stats.state == :draining
    assert stats.active_leases == 0
    assert stats.active_holders == 0
    assert stats.issued_leases == 1
    assert stats.issued_holders == 1
    assert stats.explicit_releases == 1
    assert stats.release_callbacks == 1
    assert stats.abandonments == 0
    assert_receive {:video_interop_lease_owner_drained, ^owner}
  end

  defp start_guarded_owner(opts \\ []) do
    assert {:ok, {dispatcher, _probe}} = SchemaNative.start_dispatcher()
    on_exit(fn -> assert SchemaNative.shutdown_dispatcher(dispatcher) == {:ok, true} end)
    test_pid = self()

    factory = fn owner, token, holder ->
      SchemaNative.new_abandonment_guard(dispatcher, owner, token, holder)
    end

    {:ok, owner} =
      LeaseOwner.start_link(
        [
          producer: self(),
          abandonment_guard_factory: factory,
          release: fn backend ->
            send(test_pid, {:backend_released, backend})
            :ok
          end
        ] ++ opts
      )

    {owner, dispatcher}
  end

  defp frame(fd, lease) do
    %Frame{
      coded_width: 640,
      coded_height: 480,
      visible_rect: %Rect{x: 0, y: 0, width: 640, height: 480},
      storage: %Descriptor{
        objects: [%Object{fd: fd, size: 460_800, modifier: 0}],
        layers: [
          %Layer{
            fourcc: FourCC.nv12(),
            planes: [
              %Plane{object_index: 0, offset: 0, pitch: 640},
              %Plane{object_index: 0, offset: 307_200, pitch: 640}
            ]
          }
        ]
      },
      acquire_sync: :implicit,
      lease: lease
    }
  end

  defp eventually(assertion, attempts \\ 200)
  defp eventually(assertion, 0), do: assert(assertion.())

  defp eventually(assertion, attempts) do
    if assertion.() do
      :ok
    else
      Process.sleep(2)
      eventually(assertion, attempts - 1)
    end
  end
end
