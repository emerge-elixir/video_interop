defmodule VideoInterop.DispatcherLifecycleTest do
  use ExUnit.Case, async: false

  alias VideoInterop.SchemaNative

  test "startup failure is an ordinary observable pre-publication error" do
    assert {:error,
            "video-interop release dispatcher unavailable: injected dispatcher startup failure"} =
             SchemaNative.fail_dispatcher_startup()
  end

  test "lifecycle shutdown closes, drains, and joins the dispatcher" do
    assert {:ok, {dispatcher, probe}} = SchemaNative.start_dispatcher()
    assert SchemaNative.dispatcher_health(probe) == "healthy"
    assert SchemaNative.shutdown_dispatcher(dispatcher) == {:ok, true}
    eventually(fn -> SchemaNative.dispatcher_health(probe) == "stopped" end)
  end

  test "repeated explicit close leaves no dispatcher worker threads" do
    if File.dir?("/proc/self/task") do
      Enum.each(1..20, fn _iteration ->
        assert {:ok, {dispatcher, _probe}} = SchemaNative.start_dispatcher()
        assert SchemaNative.shutdown_dispatcher(dispatcher) == {:ok, true}
      end)

      eventually(fn -> dispatcher_thread_count("vi-schema-prod") == 0 end)
    end
  end

  test "close timeout covers FIFO drain and retry joins the finished worker" do
    assert {:ok, {dispatcher, probe}} = SchemaNative.start_dispatcher()
    assert SchemaNative.delay_dispatcher_for_test(dispatcher, 250) == {:ok, true}

    started_at = System.monotonic_time(:millisecond)

    assert {:error,
            "video-interop release dispatcher unavailable: timed out waiting for dispatcher worker to drain"} =
             SchemaNative.shutdown_dispatcher_timeout(dispatcher, 10)

    assert System.monotonic_time(:millisecond) - started_at < 150
    assert SchemaNative.dispatcher_health(probe) == "stopping"
    assert SchemaNative.shutdown_dispatcher(dispatcher) == {:ok, true}
    assert SchemaNative.dispatcher_health(probe) == "stopped"
  end

  test "concurrent close calls enqueue one stop marker and all observe the join" do
    Enum.each(1..100, fn _iteration ->
      assert {:ok, {dispatcher, probe}} = SchemaNative.start_dispatcher()

      closers =
        Enum.map(1..4, fn _index ->
          Task.async(fn ->
            receive do
              :close -> SchemaNative.shutdown_dispatcher(dispatcher)
            end
          end)
        end)

      Enum.each(closers, &send(&1.pid, :close))
      assert Enum.map(closers, &Task.await(&1, 1_000)) == List.duplicate({:ok, true}, 4)
      assert SchemaNative.dispatcher_health(probe) == "stopped"
    end)
  end

  test "post-publication enqueue failure aborts instead of continuing unsafely" do
    {output, status} = run_fatal_fixture("enqueue")
    assert status != 0
    assert output =~ "video_interop fatal dispatcher corruption"
  end

  test "dispatcher worker panic aborts instead of continuing unsafely" do
    {output, status} = run_fatal_fixture("panic")
    assert status != 0
    assert output =~ "video_interop fatal dispatcher corruption"
  end

  test "a joined dispatcher stays pinned but late guards are inert" do
    script = """
    parent = self()
    {:ok, {dispatcher, _probe}} = VideoInterop.SchemaNative.start_dispatcher()
    token = make_ref()
    holder_ref = make_ref()
    holder = spawn(fn ->
      {:ok, guard} = VideoInterop.SchemaNative.new_abandonment_guard(
        dispatcher,
        parent,
        token,
        holder_ref
      )
      send(parent, {:guard_live, self()})
      Process.sleep(:infinity)
      _keep_guard_live = guard
    end)
    receive do {:guard_live, ^holder} -> :ok after 1_000 -> exit(:guard_timeout) end
    {:ok, true} = VideoInterop.SchemaNative.shutdown_dispatcher(dispatcher)
    true = :code.delete(VideoInterop.SchemaNative)
    monitor = Process.monitor(holder)
    Process.exit(holder, :kill)
    receive do {:DOWN, ^monitor, :process, ^holder, :killed} -> :ok after 1_000 -> exit(:down_timeout) end
    receive do
      {:video_interop_abandoned, ^token, ^holder_ref} -> exit(:unexpected_late_abandonment)
    after
      100 -> :ok
    end
    """

    {output, status} = run_script(script)
    assert status == 0, output
  end

  test "a foreign NIF claim pins producer resources through process death and purge" do
    script = ~S"""
    parent = self()
    thread_alive? = fn name ->
      case File.ls("/proc/self/task") do
        {:ok, tasks} ->
          Enum.any?(tasks, fn task ->
            case File.read(Path.join(["/proc/self/task", task, "comm"])) do
              {:ok, comm} -> String.trim(comm) == name
              _other -> false
            end
          end)

        _other ->
          false
      end
    end

    eventually = fn assertion ->
      Enum.reduce_while(1..200, false, fn _attempt, _acc ->
        if assertion.() do
          {:halt, true}
        else
          Process.sleep(5)
          {:cont, false}
        end
      end)
    end

    {:ok, {consumer_dispatcher, consumer_probe}} =
      VideoInterop.SchemaConsumerNative.start_dispatcher()

    producer = spawn(fn ->
      {:ok, {producer_dispatcher, _producer_probe}} =
        VideoInterop.SchemaNative.start_dispatcher()
      {:ok, {fd, fd_resource}} = VideoInterop.SchemaNative.open_test_fd()
      token = make_ref()
      holder = make_ref()
      {:ok, guard} = VideoInterop.SchemaNative.new_abandonment_guard(
        producer_dispatcher,
        parent,
        token,
        holder
      )

      lease = %VideoInterop.Lease{
        owner: parent,
        token: token,
        holder: holder,
        abandonment_guard: guard
      }

      frame = %VideoInterop.Frame{
        coded_width: 640,
        coded_height: 480,
        visible_rect: %VideoInterop.Rect{x: 0, y: 0, width: 640, height: 480},
        storage: %VideoInterop.DMABuf.Descriptor{
          objects: [%VideoInterop.DMABuf.Object{fd: fd, size: 460_800, modifier: 0}],
          layers: [
            %VideoInterop.DMABuf.Layer{
              fourcc: VideoInterop.DMABuf.FourCC.nv12(),
              planes: [
                %VideoInterop.DMABuf.Plane{object_index: 0, offset: 0, pitch: 640},
                %VideoInterop.DMABuf.Plane{
                  object_index: 0,
                  offset: 307_200,
                  pitch: 640
                }
              ]
            }
          ]
        },
        acquire_sync: :implicit,
        lease: lease
      }

      true = VideoInterop.SchemaConsumerNative.guard_is_opaque_resource(frame)
      {:ok, claim} =
        VideoInterop.SchemaConsumerNative.claim_frame(frame, consumer_dispatcher)
      {:ok, true} = VideoInterop.SchemaNative.shutdown_dispatcher(producer_dispatcher)
      send(parent, {:foreign_claim, self(), claim, token, holder})
      _pin_complete_producer_terms_until_exit = {frame, fd_resource}
    end)

    producer_monitor = Process.monitor(producer)
    receive do
      {:foreign_claim, ^producer, claim, token, holder} ->
        receive do
          {:DOWN, ^producer_monitor, :process, ^producer, :normal} -> :ok
        after
          1_000 -> exit(:producer_down_timeout)
        end

        if thread_alive?.("vi-schema-prod"), do: exit(:producer_dispatcher_not_joined)
        unless thread_alive?.("vi-schema-cons"), do: exit(:consumer_dispatcher_not_alive)
        unless :code.delete(VideoInterop.SchemaNative), do: exit(:producer_module_delete_failed)
        _purge_result = :code.purge(VideoInterop.SchemaNative)
        false = :code.is_loaded(VideoInterop.SchemaNative)

        receive do
          {:video_interop_release, ^token, ^holder} -> exit(:early_release)
          {:video_interop_abandoned, ^token, ^holder} -> exit(:early_fallback)
        after
          100 -> :ok
        end

        {:ok, true} = VideoInterop.SchemaConsumerNative.retire_claim(claim)
        receive do
          {:video_interop_release, ^token, ^holder} -> :ok
        after
          1_000 -> exit(:release_timeout)
        end
        receive do
          {:video_interop_abandoned, ^token, ^holder} -> exit(:unexpected_late_fallback)
        after
          100 -> :ok
        end
    after
      1_000 -> exit(:claim_timeout)
    end

    {:ok, true} =
      VideoInterop.SchemaConsumerNative.shutdown_dispatcher(consumer_dispatcher)
    true = eventually.(fn ->
      VideoInterop.SchemaConsumerNative.dispatcher_health(consumer_probe) == "stopped"
    end)
    true = eventually.(fn -> not thread_alive?.("vi-schema-prod") end)
    true = eventually.(fn -> not thread_alive?.("vi-schema-cons") end)
    """

    {output, status} = run_script(script)
    assert status == 0, output
  end

  test "dropping an unjoined owner fails fast without joining in its destructor" do
    root = File.cwd!()

    script = """
    {:ok, {dispatcher, _probe}} = VideoInterop.SchemaNative.start_dispatcher()
    dispatcher = nil
    :erlang.garbage_collect()
    Process.sleep(1_000)
    """

    {output, status} = run_script(script, root)
    assert status != 0
    assert output =~ "dispatcher owner dropped without explicit close_and_join"
  end

  defp dispatcher_thread_count(name) do
    "/proc/self/task/*/comm"
    |> Path.wildcard()
    |> Enum.count(fn path ->
      case File.read(path) do
        {:ok, comm} -> String.trim(comm) == name
        _error -> false
      end
    end)
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

  defp run_fatal_fixture(mode) do
    root = File.cwd!()

    script =
      case mode do
        "enqueue" ->
          """
          {:ok, {dispatcher, _probe}} = VideoInterop.SchemaNative.start_dispatcher()
          token = make_ref()
          holder = make_ref()
          VideoInterop.SchemaNative.fatal_enqueue_after_publication(
            dispatcher,
            self(),
            token,
            holder
          )
          """

        "panic" ->
          """
          {:ok, {dispatcher, _probe}} = VideoInterop.SchemaNative.start_dispatcher()
          VideoInterop.SchemaNative.fatal_worker_panic(dispatcher)
          Process.sleep(:infinity)
          """
      end

    run_script(script, root)
  end

  defp run_script(script, root \\ File.cwd!()) do
    System.cmd(
      "sh",
      ["-c", "ulimit -c 0; exec mix run --no-compile -e \"$SCRIPT\""],
      cd: root,
      env: [
        {"MIX_ENV", "test"},
        {"ERL_CRASH_DUMP", "/dev/null"},
        {"SCRIPT", script}
      ],
      stderr_to_stdout: true
    )
  end
end
