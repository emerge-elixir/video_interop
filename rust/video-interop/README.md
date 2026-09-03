# video-interop

`video-interop` provides Rust types for owned binary and borrowed Linux video frames.

It describes binary bytes and strides, DMA-BUF objects and planes, sync-file
acquire fences, frame geometry, and stream formats. Optional modules connect
those types to Rustler, EGL, and Vulkan.

The crate does not allocate buffers, create a renderer, or choose a streaming
framework.

## Status

The core, Rustler, EGL, and Vulkan APIs are part of the supported 0.1 contract.

The crate supports Rust 1.91 and newer.

## Installation

Rustler support is enabled by default:

```toml
[dependencies]
video-interop = "0.1"
```

A native program that only needs the frame types can disable it:

```toml
[dependencies]
video-interop = { version = "0.1", default-features = false }
```

Optional graphics helpers are separate features:

```toml
video-interop = { version = "0.1", features = ["egl"] }
video-interop = { version = "0.1", features = ["vulkan"] }
```

## Binary storage

`BinaryStorage` owns its bytes and plane metadata. Cloning or decoding it copies
the bytes, so it needs neither file-descriptor duplication nor a release lease.
Binary formats include RGBA8888, RGB888, Gray8, Gray2, and BW1.

## DMA-BUF ownership

`Descriptor` contains borrowed integer file descriptors. Validate it before use.
Call `Descriptor::duplicate_cloexec` before keeping it after the current call.
The returned `OwnedDescriptor` closes those duplicates when dropped.

`dmabuf_allocation_size` reads the complete allocation size exposed by a
DMA-BUF fd. This can be larger than the bytes addressed by the image planes due
to exporter alignment or driver padding. Producers should put this size in
`Object::size`. Vulkan imports compare the published size with the fd before
using it.

A sync-file descriptor follows the same rule: it is borrowed until duplicated
into an owned frame.

## Rustler frames and leases

The default feature maps both storage kinds to the structs in the
`video_interop` Hex package. It also provides prepared and claimed frames for
moving either storage kind into native queues.

`Frame::prepare_cloexec` validates a frame. It copies owned binary bytes without
acquiring a lease client and duplicates borrowed descriptors with `CLOEXEC`. If
a prepared borrowed frame is dropped, the descriptors close but lease ownership
stays with the Elixir caller.

`PreparedVideoFrame::claim` moves any lease release responsibility to native
code. Owned binary claims carry no lease. Dropping a borrowed
`ClaimedVideoFrame` or `ClaimedLease` sends the release through a
`ReleaseDispatcher`.

The NIF lifecycle owner must call `ReleaseDispatcher::close_and_join` from a
dirty I/O NIF after all claims have drained. Dispatcher and guard destructors do
not join worker threads or send through `OwnedEnv`.

## EGL

The `egl` feature loads native-fence function pointers supplied by the caller.
It supports:

- native sync-file fence creation and import;
- fence duplication with `FD_CLOEXEC`;
- server waits and bounded client waits;
- a bounded `poll` fallback when EGL import is unavailable.

The caller must keep the EGL display, context, function pointers, and calling
thread valid. The module has no link-time EGL or GL dependency.

## Vulkan

The `vulkan` feature accepts a caller-owned `ash` device and provides DMA-BUF
capability checks, memory import, sync-file waits, queue-family ownership
transfer, and release-fence handling.

Imported images stay in a bounded source cache. With planar preference,
non-linear NV12 is imported as a transfer-source image and copied plane-for-plane
into ordinary renderer-owned Y and UV images; linear NV12 uses an imported
transfer-source buffer. Packed RGBA and BGRA can use a compute copy when a
producer-linear image cannot be sampled. Copy regions cover image bytes only;
allocation padding remains mapped but is not copied.

The caller remains responsible for:

- physical-device and queue selection;
- serializing host access to the Vulkan queue;
- choosing a supported import strategy;
- renderer waits and release submission;
- presentation and device-loss recovery.

Platform-specific qualification is tracked separately from the Vulkan API's
supported status.

## Features

| Feature | Default | Provides |
| --- | --- | --- |
| `rustler` | yes | Elixir schema codecs, prepared/claimed frames, release dispatcher |
| `egl` | no | Dynamically loaded EGL native-fence helpers |
| `vulkan` | no | DMA-BUF import and synchronization helpers |
| `test-support` | no | Rustler helpers used by schema tests |

## License

Apache-2.0. See the
[license](https://github.com/emerge-elixir/video_interop/blob/v0.1.0/rust/video-interop/LICENSE).
