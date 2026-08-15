# video-interop

Owned video frame descriptors, synchronization handles, and optional Rustler
schema integration.

The crate contains framework-neutral core types and, with its default `rustler`
feature, exact stream-format, colorimetry, frame, DMA-BUF, sync-file, and lease
encoders/decoders for the `VideoInterop` Hex package. `Format` preserves every
Elixir stream field, including modifier/acquire policies and unspecified color
values. Disable default features for native-only descriptor ownership:

```toml
video-interop = { version = "0.1", default-features = false }
```

Version 0.1 supports process-local Linux DMA-BUF descriptors and acquire
sync-file fences. Its optional Rustler module also provides lifecycle-owned
release dispatchers, thin per-holder abandonment-guard construction, and
prepared/claimed lease types that preserve the complete authority envelope
through native retirement. A lifecycle-owned dirty-I/O NIF explicitly calls
`ReleaseDispatcher::close_and_join` after exact claim drainage. Resource and
guard destructors never wait, join, or send `OwnedEnv` messages; dropping an
unjoined final owner is fatal rather than detaching executable NIF code.

The optional `egl` feature provides dynamically loaded EGL native-fence
capability, import/export, typed wait, and bounded poll helpers without a
mandatory EGL/GL link dependency.

The optional `vulkan` feature provides a renderer-neutral DMA-BUF importer over
an application-selected `ash` device. It owns modifier and external-memory
queries, CLOEXEC FD import, temporary `SYNC_FD` waits, core external queue-family
transfers, and release-fence retirement. Directly sampleable formats remain
zero-copy. For linear NV12 on devices such as V3DV, the preferred `auto` path
imports the exact allocation as a transfer-source buffer and copies two explicit
pitch/offset-bounded regions. It first uses one persistent optimal multi-planar NV12
image with Vulkan sampler YCbCr conversion. If the driver cannot provide exact
linear chroma reconstruction, the same transfer fills separate persistent optimal
`R8_UNORM`/`R8G8_UNORM` images for the renderer's exact YUV shader. The established
`R32_UINT` uniform-texel-buffer compute path remains the planar rollback, followed
by a 2×2 RGBA compute fallback.

Packed RGBA/BGRA imports are also persistent. Callers select the exact Vulkan
byte order, publish the complete DMA-BUF allocation size, and provide one-plane
offset/pitch topology. Direct imports validate both the packed span and Vulkan
image-memory requirement. When a linear image is not sampleable, the explicit
`LinearBufferToOptimalBgra` strategy imports the same allocation as a bounded
`R32_UINT` uniform texel buffer and compute-copies exact bytes into a mutable
optimal BGRA image through an `R32_UINT` storage view. It does not declare
transfer usage on the producer, and XRGB's ignored byte is forced opaque. Both
strategies cache by stream incarnation plus DMA-BUF identity and complete
topology, reject active reuse/collisions, and evict only idle entries.

NV12 source imports are persistently cached by stream incarnation, DMA-BUF
`(st_dev, st_ino)`, complete allocation size, modifier, exact plane topology, and
selected read strategy. Active reappearance and topology/strategy collisions fail
closed, eviction is idle-only, and renderer-native outputs use a bounded persistent
pool. Transfer staging requires four-byte-aligned plane offsets, sizes the logical
Vulkan source buffer to the last copied plane byte, and imports the complete, potentially
larger producer allocation. This allows a truthful producer-owned driver read-ahead tail
without copying it. Exact `VkBufferImageCopy` row lengths and plane extents never copy
padding. A dedicated source fence proves staging plus return to
`QUEUE_FAMILY_EXTERNAL`, allowing the producer lease to retire before
composition/presentation. Reusable synchronization
lanes retain command pools, command buffers, fences, temporary-import semaphores,
and nonblocking timestamp queries; renderer-owned ready semaphores are never
pooled after handoff. The path never maps the producer allocation, waits on the
CPU during ordinary frames, or introduces EGL/GL dependencies. Skia,
window-system, KMS, color conversion, and device-selection integration remain
the renderer's responsibility.
