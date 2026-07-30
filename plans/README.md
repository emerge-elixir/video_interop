# Plans

- [`library-owned-video-lifecycle.md`](library-owned-video-lifecycle.md) —
  approved architecture and detailed implementation plan that moves frame
  consumption, retirement, renderer draining, direct Emerge connections, and
  reusable Membrane sinks out of applications and into the libraries.
- [`membrane-video-interop-migration.md`](membrane-video-interop-migration.md) —
  coordinated migration from the unpublished `membrane_dmabuf` contract to the
  `membrane_video_interop` adapter over `VideoInterop`. Its Phase 2 and consumer
  integration details are refined by the library-owned lifecycle plan.
