# VideoInterop

[![Hex.pm](https://img.shields.io/hexpm/v/video_interop.svg)](https://hex.pm/packages/video_interop)
[![HexDocs](https://img.shields.io/badge/hex-docs-lightgreen.svg)](https://hexdocs.pm/video_interop)
[![crates.io](https://img.shields.io/crates/v/video-interop.svg)](https://crates.io/crates/video-interop)
[![CI](https://github.com/emerge-elixir/video_interop/actions/workflows/ci.yml/badge.svg)](https://github.com/emerge-elixir/video_interop/actions/workflows/ci.yml)

## The problem

A zero-copy Linux GPU pipeline keeps video data in DMA-BUF allocations and
passes file descriptors between native producers and consumers. A file
descriptor alone does not define the image. Every frame also requires plane
layout, pixel format, modifier, geometry, colorimetry, synchronization, and a
lifetime contract.

Rust elements produce and consume the GPU frames while Elixir coordinates the
pipeline. Copying a frame into a BEAM binary transfers the pixels into CPU-owned
memory and breaks zero-copy operation. Passing a borrowed file descriptor
without ownership tracking either releases the producer buffer while a consumer
uses it or prevents the producer from reclaiming it.

## The solution

VideoInterop defines matching Elixir and Rust representations for complete video
frames. It provides:

- owned binary frames for CPU data;
- borrowed Linux DMA-BUF frames for zero-copy GPU data;
- explicit image layout, colorimetry, and synchronization metadata;
- validation and file-descriptor duplication for native consumers;
- leases, fan-out, release, and consumer-session ownership primitives;
- optional Rustler, EGL, and Vulkan integration APIs.

These primitives let programmers build zero-copy GPU pipelines in Elixir with
producer and consumer elements written in Rust. Framework adapters carry the
same `%VideoInterop.Frame{}` values without replacing the storage or ownership
contract. The `membrane_video_interop` library exposes that contract through
Membrane source and sink elements.

VideoInterop does not allocate buffers, initialize a graphics API, render, or
select an application transport.

## Project status

Version 0.1 supports owned RGBA8888, RGB888, Gray8, Gray2, and BW1 binaries,
plus Linux DMA-BUF frames and sync-file acquire fences.

The frame, binary, DMA-BUF, validation, lease, Rustler, EGL, and Vulkan APIs are
the supported 0.1 contract.

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

Use the Rust crate directly in native code:

```toml
[dependencies]
video-interop = "0.1"
```

The crate enables Rustler support by default. Disable it for a Rust-only
consumer:

```toml
video-interop = { version = "0.1", default-features = false }
```

## Describing an owned frame

Use `VideoInterop.Frame.binary/2` for immutable BEAM data:

```elixir
frame =
  VideoInterop.Frame.binary(rgba,
    width: 640,
    height: 480,
    pixel_format: :rgba8888,
    framerate: {30, 1}
  )

:ok = VideoInterop.validate(frame)
:ok = VideoInterop.release(frame)
```

The binary is owned by the frame term, synchronization is implicit, and
`release/1` is a no-op. RGBA8888 defaults to premultiplied alpha; pass
`alpha_mode: :straight` for ordinary straight-alpha bytes. Packed Gray2 and BW1
rows are MSB-first and independently
strided. BW1 also requires `bw1_polarity: :one_is_black` or `:one_is_white`.

## Describing a borrowed frame

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

`Object.size` is the complete allocation size reported by the DMA-BUF fd. The
size includes any exporter alignment and driver padding outside the bytes
addressed by the image planes.

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

## Leases for borrowed storage

A lease gives one holder the right to use a producer buffer. Binary frames do
not use leases. Release tells the
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

The default `rustler` feature decodes binary and DMA-BUF frame and format
structs. Binary storage decodes into owned bytes. A native consumer must
duplicate borrowed file descriptors before keeping a DMA-BUF frame after the
NIF call returns.

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

Preparing an owned binary frame copies its bytes and does not acquire a lease
client. Preparing borrowed storage duplicates its descriptors but leaves the
lease with the Elixir caller. Claiming a borrowed frame moves release
responsibility to native code; claiming an owned frame carries no lease.

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

Enable the Vulkan module with:

```toml
video-interop = { version = "0.1", features = ["vulkan"] }
```

The Vulkan module imports DMA-BUF objects through a caller-provided `ash`
device. It checks fd allocation sizes, modifier support, queue ownership, and
sync-file waits. It also contains bounded fallback paths for linear NV12 and
packed images when a device cannot sample the producer image directly.

The caller owns device selection, queue serialization, renderer integration,
and final presentation. Platform-specific qualification does not change the
Vulkan module's supported API status.

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

Maintainers follow the
[release checklist](https://github.com/emerge-elixir/video_interop/blob/main/RELEASING.md)
for package checks and publication order.

## License

Apache-2.0. See [LICENSE](LICENSE).
