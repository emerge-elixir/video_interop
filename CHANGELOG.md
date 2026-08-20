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
- Separate the NV12 transfer buffer's exact copied-byte span from the complete imported allocation
  size so truthful producer-owned V3DV read-ahead tails satisfy Vulkan memory requirements without
  entering any copy region.
- Isolate host-built Rust schema test NIFs under the ignored Cargo target directory so they
  cannot leak into a Nerves target release through the application's `priv` directory.
- Reserve finite lease capacity before the token-bearing issue commit, run backend release
  callbacks on a monitored serial executor, normalize owner-death lifecycle calls, and keep
  active-holder/oldest-lease statistics incremental.
- Count dispatcher delivery to dead local owners separately from fatal worker/channel corruption.
- Verify DMA-BUF allocation sizes against the fd, reject unreferenced descriptor objects, and bound
  compute NV12 shader addressing to its logical 32-bit source span.
- Bind Vulkan synchronization lanes to unique import identities, enforce renderer wait/release
  ordering, route queue submission through renderer-owned authority, and quarantine uncertain drop.
- Inventory and resolve multiple NV12 candidates per modifier, preserve forced fail-closed modes,
  and cache direct NV12 imports alongside staged sources.
- Add reproducible SPIR-V regeneration/validation and all-feature Vulkan CI coverage.
