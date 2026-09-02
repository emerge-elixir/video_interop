# Plans

- [`release-0.1.0-preparation.md`](release-0.1.0-preparation.md) — phased plan to fix all
  initial-release blockers, strengthen full Elixir/Rust CI, publish both artifacts, and migrate
  downstream projects to registry-only dependencies.
- [`release-0.1.0-audit.md`](release-0.1.0-audit.md) — initial Hex/crates.io release-readiness,
  packaging, CI, registry, documentation, and qualification audit.
- [`video-interop-audit.md`](video-interop-audit.md) — bottom-up safety, lifecycle, Vulkan,
  testing, and maintainability audit with prioritized findings.
- [`video-interop-audit-remediation.md`](video-interop-audit-remediation.md) — detailed phased
  implementation, migration, testing, target qualification, and rollback plan for every audit
  finding.
- [`library-owned-video-lifecycle.md`](library-owned-video-lifecycle.md) —
  historical lifecycle design. Its generic ownership work remains; its
  renderer-owned Emerge targets, sessions, and connection APIs were superseded
  by direct atom-target frame submission.
- [`membrane-video-interop-migration.md`](membrane-video-interop-migration.md) —
  historical migration design from the unpublished `membrane_dmabuf` contract.
  The completed transport uses `Membrane.VideoInterop.Source` and `Sink`.
