# Changelog

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
