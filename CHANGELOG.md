# Changelog

## 0.1.0 - Unreleased

- Add the framework-neutral `VideoInterop` Elixir frame, format, DMA-BUF,
  sync-file, validation, and lease contracts.
- Add the single `video-interop` Rust crate with optional Rustler schemas,
  close-on-exec fd duplication, prepared/claimed lease ownership, and RAII
  cleanup.
- Reserve EGL and Vulkan integrations for optional features in the same crate.
