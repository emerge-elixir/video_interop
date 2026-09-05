# ARMv7 portability / 0.1.1

- [x] Reproduce the Emerge OpenGL failure with an ARMv7 Cargo check.
- [x] Use Linux's 64-bit stat/seek APIs for complete allocation sizes and inode
  identities on both 32-bit and 64-bit targets; avoid casts or lint suppression.
- [x] Add large sparse-file coverage and ARMv7 compile/Clippy CI checks.
- [x] Prepare matching Cargo/Hex 0.1.1 patch-release metadata and validate packages.
- [x] Validation: ARMv7 all-target/all-feature checks and warning-denied Clippy
  on Rust 1.91 and stable; AArch64 check; host Rust tests and 113 Elixir tests;
  documentation, workflow lint, and Hex/Cargo source parity all pass.
- [x] Emerge's 1,007 Rust tests plus the fixture test pass against local 0.1.1
  using a temporary command-line Cargo patch. Restore Emerge's registry lock
  afterward; no local patch remains. Emerge's 455 default Elixir tests pass.
- [ ] Publish through the existing protected exact-tag CI (not locally).
- [ ] Update Emerge's registry dependency and Cargo.lock after 0.1.1 is available.
- [ ] Rerun the ARMv7 OpenGL artifact job.

Emerge must not keep using the broken crates.io 0.1.0 archive or depend on a
local path patch in a release artifact build. A local path override may be used
only for validation before publication.
