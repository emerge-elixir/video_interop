# Changelog

## 0.1.0 - Unreleased

- Add the framework-neutral `VideoInterop` Elixir frame, format, DMA-BUF,
  sync-file, validation, and lease contracts.
- Add the single `video-interop` Rust crate with optional Rustler schemas,
  close-on-exec fd duplication, prepared/claimed lease ownership, and RAII
  cleanup.
- Add optional dynamically loaded EGL native sync-file fencing without mandatory EGL/GL linkage; reserve Vulkan for a later feature.
- Add explicit caller-owned/transferred issue results, atomic drain waiters,
  normalized retry errors, and optional single-flight exponential release retry.
- Add frame-level retain and ownership-aware consumer/session protocols with
  consuming open, transfer, and close helpers.
- Add prototype-gated per-holder abandonment guards, transactional root/retain
  construction, bounded release tombstones, honest late-release diagnostics,
  distinct fallback accounting, and immutable final drain stats.
- Authenticate producer-native guards through authority envelopes, preserve
  them opaquely through Rust prepare/claim, and replace the detached static
  worker with explicitly closed/joined lifecycle dispatchers whose destructors
  remain nonblocking.
- Add persistent direct packed Vulkan imports plus explicit bounded linear-buffer-to-optimal-BGRA
  compute staging for devices that cannot sample producer-linear images.
- Add bounded linear-buffer-to-optimal NV12 transfer staging with exact plane copy regions,
  multi-planar sampler-YCbCr output where exact filtering exists, separate optimal Y/UV transfer
  output otherwise, explicit external ownership return, and compute-planar rollback.
- Isolate host-built Rust schema test NIFs under the ignored Cargo target directory so they
  cannot leak into a Nerves target release through the application's `priv` directory.
