defmodule VideoInterop.AbandonmentGuardAuthorityTest do
  use ExUnit.Case, async: false

  alias VideoInterop.{AbandonmentGuard, LeaseOwner, SchemaNative}

  test "bare and wrapped ordinary references are not native guards" do
    refute AbandonmentGuard.valid?(make_ref())

    refute AbandonmentGuard.valid?(%AbandonmentGuard{
             resource: make_ref(),
             authority: SchemaNative
           })
  end

  test "producer authority accepts only its own live resource type" do
    assert {:ok, {dispatcher, _probe}} = SchemaNative.start_dispatcher()
    on_exit(fn -> assert SchemaNative.shutdown_dispatcher(dispatcher) == {:ok, true} end)

    assert {:ok, guard} =
             SchemaNative.new_abandonment_guard(
               dispatcher,
               self(),
               make_ref(),
               make_ref()
             )

    assert AbandonmentGuard.valid?(guard)

    refute AbandonmentGuard.valid?(%{guard | resource: make_ref()})
    refute AbandonmentGuard.valid?(%{guard | authority: __MODULE__})
  end

  test "LeaseOwner rejects a fake guard before publishing a holder" do
    test_pid = self()

    assert {:ok, owner} =
             LeaseOwner.start_link(
               producer: self(),
               abandonment_guard_factory: fn _owner, _token, _holder ->
                 {:ok,
                  %AbandonmentGuard{
                    resource: make_ref(),
                    authority: SchemaNative
                  }}
               end,
               release: fn backend ->
                 send(test_pid, {:released, backend})
                 :ok
               end
             )

    assert {:error, {:transferred, {:abandonment_guard_factory_failed, {:invalid_guard, _guard}}}} =
             LeaseOwner.issue(owner, :fake_guard)

    assert_receive {:released, :fake_guard}
    assert :ok = LeaseOwner.close(owner)
  end
end
