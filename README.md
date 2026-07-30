# VideoInterop

`video_interop` is a lightweight, framework-neutral contract for borrowing
video frames across Elixir and native Rust code.

The project publishes one Hex package (`video_interop`) and one Rust crate
(`video-interop`) from the same repository. The Hex archive also includes the
crate source. Core ownership, Rustler schema support, and future EGL/Vulkan
adapters stay in that one crate; graphics API
integrations are feature-gated rather than split into separate packages.

Version 0.1 supports:

- Linux DMA-BUF object/layer/plane descriptors modeled after
  `AVDRMFrameDescriptor`;
- DRM fourcc and explicit/implicit modifier metadata;
- implicit acquire synchronization and borrowed Linux sync-file fences;
- close-on-exec native fd duplication and RAII cleanup;
- visible/coded frame geometry and generic format metadata;
- deterministic BEAM leases with safe fan-out and draining;
- exact optional Rustler encoding/decoding for frame, DMA-BUF, sync-file, and
  lease boundary structs.

It does not allocate buffers, initialize graphics APIs, render, present, or
define a streaming-framework transport.

## Elixir contract

```elixir
alias VideoInterop.{Format, Frame, LeaseOwner, Rect}
alias VideoInterop.DMABuf
alias VideoInterop.DMABuf.{Descriptor, FourCC, Layer, Object, Plane}

{:ok, lease_owner} =
  LeaseOwner.start_link(
    producer: self(),
    release: fn backend_token -> Producer.release(backend_token) end,
    max_active: 4
  )

{:ok, lease} = LeaseOwner.issue(lease_owner, backend_token)

descriptor = %Descriptor{
  objects: [%Object{fd: fd, size: 5_529_600, modifier: 0}],
  layers: [
    %Layer{
      fourcc: FourCC.nv12(),
      planes: [
        %Plane{object_index: 0, offset: 0, pitch: 2560},
        %Plane{object_index: 0, offset: 3_686_400, pitch: 2560}
      ]
    }
  ]
}

frame = %Frame{
  coded_width: 2560,
  coded_height: 1440,
  visible_rect: %Rect{x: 0, y: 0, width: 2560, height: 1440},
  storage: descriptor,
  acquire_sync: :implicit,
  lease: lease
}

format = %Format{
  width: 2560,
  height: 1440,
  framerate: {60, 1},
  storage: %DMABuf.Format{fourcc: FourCC.nv12(), modifier: :per_buffer}
}

:ok = VideoInterop.validate(frame)
:ok = VideoInterop.validate(frame, format)
```

Timestamps belong to the transport carrying the frame. The separate
`membrane_video_interop` package places `%VideoInterop.Frame{}` in a
`Membrane.Buffer` without making this package depend on Membrane.

## Ownership

File descriptor integers in Elixir are borrowed and process-local. They cannot
be serialized, sent to another Erlang node, or retained after release.
Cross-process transport requires OS handle transfer such as `SCM_RIGHTS`.

A native consumer must:

1. structurally validate the frame;
2. duplicate every object and acquire-fence fd before asynchronous use;
3. claim native lease ownership only after accepting the frame;
4. keep the unique holder until exact CPU/GPU/display retirement;
5. retire or drop the claimed native lease.

`VideoInterop.LeaseOwner` makes release idempotent per `{token, holder}`, uses
confirmed issuance and fan-out retention, and drains holders after producer
shutdown. Issue errors state ownership explicitly:

```elixir
case LeaseOwner.issue(owner, backend_token) do
  {:ok, lease} ->
    publish(lease)

  {:error, {:caller_owned, reason}} ->
    Producer.release(backend_token)
    {:error, reason}

  {:error, {:transferred, reason}} ->
    {:error, reason}
end
```

The send operation is the ownership boundary. Backend tokens also require an
owner-crash/message-drop destructor fallback. A holder must never be copied to
another consumer; use `VideoInterop.retain/2` to return the same frame with a
unique child holder.

`LeaseOwner.close/2` begins draining without waiting. `LeaseOwner.drain/2`
registers a waiter atomically and returns only after all holders and final
release callbacks complete. Optional single-flight exponential release retry is
available for idempotent backend callbacks:

```elixir
release_retry:
  {:exponential, initial_ms: 10, max_ms: 1_000, max_attempts: :infinity}
```

A failed final callback remains alive and retryable after producer death and
after automatic retry exhaustion. The owner never terminates implicitly merely
because no notification observer is alive; a producer that wants a fatal policy
must configure and own that policy explicitly.

### Consumer sessions

Consumers implement `VideoInterop.Consumer` to open a format-specific session
and `VideoInterop.ConsumerSession` to transfer and close frames. The transfer
receipt identifies the holder owner at the return boundary. Applications use
the consuming helper instead of branching on receipts themselves:

```elixir
{:ok, session} = VideoInterop.open_consumer(consumer, format, owner: self())
:ok = VideoInterop.consume(session, frame)
:ok = VideoInterop.close_consumer(session)
```

After `consume/2` returns normally, the caller never releases that frame. The
helper releases known caller-owned rejection, while transferred frames retire
inside the consumer. Session close is idempotent and stops admission before
pending/current frames are retired or scheduled for consumer-safe retirement.
A contract implementation that raises or returns an invalid receipt leaves
ownership unknown, so the helper raises rather than risking a double release.

## Rust crate

The default `rustler` feature exposes exact frame/descriptor/lease decoders for
the native boundary:

```toml
[dependencies]
video-interop = "0.1"
```

```rust
use video_interop::Frame;

fn accept(frame: Frame<'_>) -> Result<(), Box<dyn std::error::Error>> {
    let prepared = frame.prepare_cloexec()?;

    // Transfer to a native queue first. Claim only when that queue accepts
    // responsibility for eventual release.
    let claimed = prepared.claim();
    drop(claimed); // closes duplicate fds and queues the generic release message
    Ok(())
}
```

Core-only native users can omit Rustler:

```toml
video-interop = { version = "0.1", default-features = false }
```

Dropping `PreparedVideoFrame` closes duplicated fds but leaves release with the
Elixir caller. Dropping `ClaimedVideoFrame` or `ClaimedLease` queues
`{:video_interop_release, token, holder}` through a dedicated native worker, so
Rustler's `OwnedEnv::send_and_clear` never runs on a BEAM scheduler thread.

## Migration plan

See the
[`membrane_video_interop` migration plan](https://github.com/emerge-elixir/video_interop/blob/main/plans/membrane-video-interop-migration.md)
for the coordinated adapter and downstream consumer cutover.

## Planned optional features

EGL native-fence and Vulkan `SYNC_FD` semaphore adapters will be added as
optional `egl` and `vulkan` features of the same `video-interop` crate. They will
accept caller-owned displays, contexts, devices, and queues; the crate will not
create rendering runtimes. Metal and Direct3D remain later work.

## Validation

```sh
mix deps.get
mix format --check-formatted
mix test
mix hex.build

cargo fmt --all -- --check
cargo test --workspace
cargo test -p video-interop --no-default-features
cargo clippy --workspace --all-targets -- -D warnings
```

## License

Apache-2.0. See [LICENSE](LICENSE).
