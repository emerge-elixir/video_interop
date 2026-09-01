# VideoInterop

[![Hex.pm](https://img.shields.io/hexpm/v/video_interop.svg)](https://hex.pm/packages/video_interop)
[![HexDocs](https://img.shields.io/badge/hex-docs-lightgreen.svg)](https://hexdocs.pm/video_interop)
[![crates.io](https://img.shields.io/crates/v/video-interop.svg)](https://crates.io/crates/video-interop)
[![CI](https://github.com/emerge-elixir/video_interop/actions/workflows/ci.yml/badge.svg)](https://github.com/emerge-elixir/video_interop/actions/workflows/ci.yml)

VideoInterop describes a borrowed video frame as plain Elixir and Rust data.

A producer can use it to hand a DMA-BUF frame to a renderer or another native
consumer without tying either side to a streaming framework. The frame carries
its storage layout, synchronization information, and a lease that tells the
producer when the consumer is finished.

VideoInterop does not allocate buffers, initialize a graphics API, render, or
choose how frames travel through an application.

## Project status

Version 0.1 supports Linux DMA-BUF frames and sync-file acquire fences.

The frame, validation, lease, Rustler, and EGL APIs are the supported 0.1
contract. The Vulkan module is experimental. Its API may change while hardware
testing on V3DV continues.

File descriptor integers are only useful inside one operating-system process.
They cannot be sent to another Erlang node. A process boundary requires an OS
handle-transfer mechanism such as `SCM_RIGHTS`.

## Installation

Add the Hex package to your dependencies:

```elixir
def deps do
  [
    {:video_interop, "~> 0.1.0"}
  ]
end
```

Native code can use the Rust crate directly:

```toml
[dependencies]
video-interop = "0.1"
```

The crate enables Rustler support by default. A Rust-only consumer can turn it
off:

```toml
video-interop = { version = "0.1", default-features = false }
```

## Describing a frame

Here is a small NV12 frame backed by one DMA-BUF object:

```elixir
alias VideoInterop.{Frame, Lease, Rect}
alias VideoInterop.DMABuf
alias VideoInterop.DMABuf.{Descriptor, FourCC, Layer, Object, Plane}

lease = Lease.new(self(), :buffer_1)

descriptor = %Descriptor{
  objects: [%Object{fd: fd, size: 384, modifier: :implicit}],
  layers: [
    %Layer{
      fourcc: FourCC.nv12(),
      planes: [
        %Plane{object_index: 0, offset: 0, pitch: 16},
        %Plane{object_index: 0, offset: 256, pitch: 16}
      ]
    }
  ]
}

frame = %Frame{
  coded_width: 16,
  coded_height: 16,
  visible_rect: %Rect{x: 0, y: 0, width: 16, height: 16},
  storage: descriptor,
  acquire_sync: :implicit,
  lease: lease
}

format = %VideoInterop.Format{
  width: 16,
  height: 16,
  framerate: {30, 1},
  storage: %DMABuf.Format{fourcc: FourCC.nv12()}
}

:ok = VideoInterop.validate(frame)
:ok = VideoInterop.validate(frame, format)
```

`Object.size` is the complete allocation size reported by the DMA-BUF fd. It
may be larger than the bytes used by the image planes because an exporter can
add alignment or driver padding.

Timestamps belong to the transport that carries the frame, so they are not part
of `%VideoInterop.Frame{}`.

## Passing a frame to a consumer

A consumer implements `VideoInterop.Consumer`. Opening it returns a value that
implements `VideoInterop.ConsumerSession`.

```elixir
{:ok, session} = VideoInterop.open_consumer(consumer, format, owner: self())
:ok = VideoInterop.consume(session, frame)
:ok = VideoInterop.close_consumer(session)
```

After `consume/2` returns normally, do not release the frame yourself. The
helper releases a frame when the consumer rejects it before taking ownership.
Once the consumer takes ownership, that consumer is responsible for release.
If a consumer raises or returns a result that does not say who owns the frame,
`consume/2` raises rather than risk releasing it twice.

`close_consumer/1` stops new transfers. A session implementation must also
finish or schedule the release of frames it already accepted.

## Leases

A lease gives one holder the right to use a producer buffer. Release tells the
producer that holder is finished.

Use one `VideoInterop.LeaseOwner` for each producer or native buffer pool:

```elixir
{:ok, owner} =
  VideoInterop.LeaseOwner.start_link(
    producer: self(),
    release: fn backend_token -> Producer.release(backend_token) end,
    max_active: 4
  )

case VideoInterop.LeaseOwner.issue(owner, backend_token) do
  {:ok, lease} ->
    publish(lease)

  {:error, {:caller_owned, reason}} ->
    Producer.release(backend_token)
    {:error, reason}

  {:error, {:transferred, reason}} ->
    {:error, reason}
end
```

A `:caller_owned` error means the token never left the caller. A `:transferred`
error means the lease owner received it and remains responsible for cleanup.

If a frame goes to more than one consumer, call `VideoInterop.retain/2` for each
extra branch. Each branch gets a different holder. Never copy a lease struct to
fan out a frame.

`LeaseOwner.close/2` stops new leases. `LeaseOwner.drain/2` waits for every
holder and release callback. Release callbacks run in a separate serial process,
so a slow callback does not block lease messages.

See the `VideoInterop.LeaseOwner` and `VideoInterop.Lease` module documentation
for retry, shutdown, and abandonment fallback behavior.

## Using a frame from Rust

The default `rustler` feature decodes the Elixir frame and format structs. A
native consumer should duplicate file descriptors before keeping a frame after
the NIF call returns.

```rust
use rustler::ResourceArc;
use video_interop::{Frame, ReleaseDispatcher};

fn accept(
    frame: Frame<'_>,
    dispatcher: &ResourceArc<ReleaseDispatcher>,
) -> Result<(), Box<dyn std::error::Error>> {
    let prepared = frame.prepare_cloexec(dispatcher)?;

    // Put the frame in the native queue before claiming its lease.
    let claimed = prepared.claim();

    // Dropping the claim closes the duplicated fds and sends the release.
    drop(claimed);
    Ok(())
}
```

Dropping a prepared frame closes its duplicated descriptors but leaves the lease
with the Elixir caller. Claiming the frame moves release responsibility to
native code.

A lifecycle owner must call `ReleaseDispatcher::close_and_join` from a dirty I/O
NIF after all native claims have drained. Destructors do not wait for the worker
or send messages through `OwnedEnv`.

## EGL and Vulkan

Enable EGL helpers with:

```toml
video-interop = { version = "0.1", features = ["egl"] }
```

The EGL module loads native-fence functions from the caller's EGL library. The
caller still owns the display, context, thread rules, and function addresses.
VideoInterop does not link EGL or GL.

Enable the experimental Vulkan module with:

```toml
video-interop = { version = "0.1", features = ["vulkan"] }
```

The Vulkan module imports DMA-BUF objects through a caller-provided `ash`
device. It checks fd allocation sizes, modifier support, queue ownership, and
sync-file waits. It also contains bounded fallback paths for linear NV12 and
packed images when a device cannot sample the producer image directly.

The caller owns device selection, queue serialization, renderer integration,
and final presentation. Vulkan support should not be treated as stable until
the pinned-RPi5 qualification work is complete.

## Development

The release CI compiles, lints, and tests the complete Rust workspace, each
optional Rust feature, both supported Elixir versions, generated packages, and
Vulkan shaders.

```sh
mix deps.get
mix format --check-formatted
mix compile --force --warnings-as-errors
mix test
mix docs --warnings-as-errors

cargo fmt --all -- --check
cargo build --workspace --all-targets --all-features
cargo build --release --workspace --all-targets --all-features
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p video-interop --all-features --no-deps

scripts/check-vulkan-shaders.sh
scripts/check-release-artifact-parity.sh
```

Maintainers can follow the
[release checklist](https://github.com/emerge-elixir/video_interop/blob/main/RELEASING.md)
for package checks and publication order.

## License

Apache-2.0. See [LICENSE](LICENSE).
