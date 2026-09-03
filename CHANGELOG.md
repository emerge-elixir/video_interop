# Changelog

## 0.1.0 - Unreleased

First public release.

### Frame contract

- Add Elixir and Rust types for owned binary and borrowed Linux DMA-BUF frames,
  including binary strides, packed grayscale interpretation, DMA-BUF layers,
  planes, modifiers, visible geometry, color information, and sync-file fences.
- Validate binary dimensions and byte bounds, descriptor bounds, object
  references, plane layout, stream format, storage ownership, and DMA-BUF
  allocation sizes.
- Prepare owned binary frames for native queues without lease clients, and
  require exact leases while duplicating borrowed descriptors with `FD_CLOEXEC`.
- Close owned file-descriptor copies when their Rust values are dropped.
- Keep timestamps in the transport rather than the frame descriptor.

### Leases and consumers

- Add `VideoInterop.LeaseOwner` for bounded frame leases, fan-out, producer
  shutdown, draining, release callbacks, and optional release retry.
- Report whether an issue error leaves the backend token with the caller or
  transfers cleanup to the lease owner.
- Add per-holder fallback guards for native producers that need cleanup when a
  holder-bearing BEAM term disappears without release.
- Add `VideoInterop.Consumer` and `VideoInterop.ConsumerSession` with helpers
  that keep frame ownership clear on open, transfer, rejection, and close.
- Add native prepared and claimed frame types backed by a release-dispatch
  worker with a required lifecycle close.

### Graphics helpers

- Add dynamically loaded EGL native-fence creation, import, duplication, waits,
  and polling without a link-time EGL or GL dependency.
- Add an experimental Vulkan module for DMA-BUF capability checks, memory
  import, sync-file waits, external queue ownership, release fences, and bounded
  import caches.
- Add renderer-owned staging for NV12 and packed RGBA/BGRA when producer
  allocations cannot be wrapped directly, including plane-for-plane copies from
  non-linear NV12 images into ordinary optimal Y and UV images.
- Keep image copy regions separate from allocation padding reported by the
  DMA-BUF fd.

### Packages and validation

- Publish the `video_interop` Hex package without a bundled NIF. The package
  includes the source for the `video-interop` crate.
- Publish the same Rust source as the `video-interop` crate with optional
  `rustler`, `egl`, and `vulkan` features.
- Test the supported Elixir and Rust versions, all Rust features, generated Hex
  and Cargo packages, and reproducible Vulkan shaders in CI.

### Known limits

- File descriptor integers are local to one OS process and cannot be sent to
  another Erlang node.
- Vulkan support is experimental until the pinned-RPi5 hardware matrix is
  complete.
- Metal and Direct3D adapters are not included.
