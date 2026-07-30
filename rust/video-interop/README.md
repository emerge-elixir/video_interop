# video-interop

Owned video frame descriptors, synchronization handles, and optional Rustler
schema integration.

The crate contains framework-neutral core types and, with its default `rustler`
feature, exact frame, DMA-BUF, sync-file, and lease encoders/decoders for the
`VideoInterop` Hex package. Disable default features for native-only descriptor
ownership:

```toml
video-interop = { version = "0.1", default-features = false }
```

Version 0.1 supports process-local Linux DMA-BUF descriptors and acquire
sync-file fences. EGL and Vulkan adapters will be optional features of this same
crate in later slices.
