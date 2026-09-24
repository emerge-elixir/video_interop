# Changelog

## 0.1.2 - 2026-09-24

### Added

- Controls to disable existing Vulkan GPU timestamp instrumentation or sample
  complete staged acquire-to-release intervals without changing synchronization.
  Existing consumers keep staged-frame timing enabled by default.
- Vulkan timing status notifications and
  `ImportedImageSync::has_gpu_timing_resources` for diagnostics.
- Driver-free regression tests for timing policies, release-fence gating,
  sync-owner reuse, and backward compatibility with existing Vulkan consumers.

### Fixed

- Fixed macOS compilation of video frame helpers while preserving 64-bit
  DMA-BUF allocation sizes and inode identities on Linux, including ARMv7.
- Made descriptor, EGL, and dispatcher lifecycle tests portable to macOS.
- Prevented out-of-memory errors during Vulkan timestamp query-pool allocation
  from failing synchronization setup. Query readback now omits timing samples
  instead of failing release-completion polling on `NOT_READY` or out-of-memory
  errors.
- Disabled Vulkan timestamp instrumentation for non-finite or non-positive
  timestamp periods and timestamp valid-bit counts greater than 64.
- Correctly mark the Vulkan context as device-lost and return a device-loss error
  when timestamp query-pool allocation or readback reports `ERROR_DEVICE_LOST`.

## 0.1.1 - 2026-09-05

### Fixed

- Fixed ARMv7 compilation of DMA-BUF probing by using 64-bit stat and seek
  APIs for allocation sizes, file positions, and inode identities.
- Added ARMv7 compile and Clippy checks to the release gate.

## 0.1.0 - 2026-09-03

First public release.

### Added

- Framework-neutral Elixir and Rust frame formats for owned binary video and
  Linux DMA-BUF video.
- Support for RGBA8888, RGB888, Gray8, Gray2, BW1, and DMA-BUF pixel formats.
- Explicit visible geometry, colorimetry, alpha, DRM modifier, and acquire-fence
  metadata.
- Validation for frame, format, binary-plane, and DMA-BUF descriptors.
- Bounded leases and consumer helpers for safe ownership transfer and release.
- Optional Rustler, EGL, and Vulkan integration APIs.

### Limitations

- DMA-BUF file descriptors are process-local and cannot be sent directly to
  another Erlang node.
- Metal and Direct3D adapters are not included.
