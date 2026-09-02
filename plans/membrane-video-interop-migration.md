# Membrane Video Interop Migration

Status: Superseded by the completed standalone transport design. Current
integration uses `Membrane.VideoInterop.Source` and `Sink`, atom video targets,
and `Emerge.submit_video_frame/3`; direct Emerge connection details below are
retained as historical context only.

## Goal

Use the new standalone `/workspace/membrane_video_interop` project as the thin
adapter, then migrate every current producer and consumer to the generic
`VideoInterop` frame, format, synchronization, and lease contract. Keep
`/workspace/colibri/membrane_dmabuf` unchanged as the old-contract recovery
point until the lockstep migration is complete.

This is a coordinated source migration. The old and new frame/lease protocols
must not be mixed at runtime.

## Contract decisions

### Package boundary

`membrane_video_interop` publishes:

- Hex app/package: `:membrane_video_interop`;
- namespace: `Membrane.VideoInterop`;
- dependency: `video_interop ~> 0.1.0`;
- no Rust crate, Rustler dependency, NIF, generic structs, validator, or lease
  implementation.

All generic functionality remains in the existing single Hex package and Rust
crate under `/workspace/video_interop`.

### Stream format

Use `%VideoInterop.Format{storage: %VideoInterop.DMABuf.Format{}}` directly as
the Membrane stream format. Membrane stream formats are ordinary structs; a
`Membrane.VideoInterop.VideoFormat` wrapper would duplicate schema and
conversion logic without adding transport behavior.

Keep producer options named `output: :dmabuf`; they still select DMA-BUF storage.

### Buffer contract

```elixir
%Membrane.Buffer{
  payload: <<>>,
  pts: pts,
  dts: dts,
  metadata: %{
    video_interop: %VideoInterop.Frame{},
    camera: producer_metadata
  }
}
```

`Membrane.VideoInterop` exposes the transport helpers:

```elixir
metadata_key/0  # => :video_interop
put_frame/2
fetch_frame/1
fetch_frame!/1
```

and a reusable `%Membrane.VideoInterop.Sink{}` that delegates frame ownership to
`VideoInterop.Consumer`. The sink is Membrane glue; it does not implement
validation, leases, descriptors, or native retirement itself.

Rules:

- `put_frame/2` requires an empty binary payload and preserves timestamps and
  unrelated metadata, except that legacy `:dmabuf` is reserved and causes an
  `ArgumentError`;
- `fetch_frame/1` returns `:error` for a nonempty payload, missing key, wrong
  value type, or a buffer containing both `:video_interop` and legacy
  `:dmabuf`;
- producers and custom elements use `VideoInterop.validate/1,2`,
  `VideoInterop.release/1`, and `VideoInterop.retain/2` directly;
- the reusable sink validates through `VideoInterop`, consumes through the
  generic consumer protocol, and releases only known caller-owned rejections;
- the adapter does not duplicate generic validation or lease implementations.

### Atomic incompatibility

The migration changes all of these together:

```text
%Membrane.DMABuf.VideoFrame{descriptor:, synchronization:}
%Membrane.DMABuf.Lease{}
:membrane_dmabuf_*
metadata.dmabuf
```

into:

```text
%VideoInterop.Frame{storage:, acquire_sync:}
%VideoInterop.Lease{}
:video_interop_*
metadata.video_interop
```

A metadata-only compatibility alias is unsafe. Rustler schemas require exact
module and field names, and an old lease owner ignores new release messages.
Never put old frames under `:video_interop`, emit both frame forms for one
holder, or retry a submission using the other contract after ownership may have
transferred.

## Phase 0: freeze safety points and the generic foundation

1. Preserve every worktree before any fix. For repositories with `HEAD`, create
   a recovery ref, binary tracked/staged diffs, untracked archive with SHA-256
   manifest, and verified `--include-untracked` stash restore.
2. For unborn `/workspace/video_interop` and
   `/workspace/membrane_video_interop`, first create complete tar archives and
   sorted path/size/SHA-256 manifests, verify byte-for-byte disposable restores,
   then make explicit recovery snapshot commits. Do not attempt refs/stashes
   before `HEAD` exists.
3. Fix and freeze `VideoInterop.LeaseOwner.issue/3` before consumers depend on
   its ownership rule:
   - monitor the owner before sending the issue request;
   - return an immediate owner-down error instead of waiting for timeout;
   - use explicit results: `{:error, {:caller_owned, reason}}` only when no
     request was sent, and `{:error, {:transferred, reason}}` after send;
   - require backend tokens to have an owner/message-drop destructor fallback;
   - test already-dead owner, death before receipt, death after registration,
     timeout/cancel, capacity, draining, and release-callback failure races.
   An `issue/3` caller releases the backend token only for the explicit
   `{:caller_owned, reason}` result. It must never release after `:ok` or a
   `{:transferred, reason}` result.
4. Commit the hardening separately from the recovery snapshot.
5. Re-run Mix, Cargo, no-default-feature, Clippy, docs, and packaging gates.
6. Tag or branch `/workspace/colibri/membrane_dmabuf` at `845a697` as the
   pre-migration recovery point.
7. Record a lockstep manifest of repository SHAs, stash IDs, archive hashes, and
   worktree patches.

Do not rename or mutate the old repository during migration staging. Consumer
branches reference only the new `/workspace/membrane_video_interop` project;
the old checkout remains an exact rollback source.

## Phase 1: implement the standalone adapter

The transport-helper foundation is implemented in
`/workspace/membrane_video_interop` as a new Git project. The reusable sink is a
pending follow-up after the `VideoInterop.Consumer` session contract lands.

### Project contents

```text
mix.exs
lib/membrane/video_interop.ex                  # implemented
lib/membrane/video_interop/sink.ex             # pending
test/buffer_contract_test.exs                  # implemented
test/sink_test.exs                             # pending
README.md
CHANGELOG.md
LICENSE
.github/workflows/ci.yml
```

### Excluded by design

Do not copy these from the old repository:

```text
lib/membrane/dmabuf.ex
lib/membrane/dmabuf/**
rust/**
Cargo.toml
Cargo.lock
test/native/**
test/support/schema_native.ex
Rust schema, descriptor, validator, format, and lease-owner tests
PIPELINE_MIGRATION_PLAN.md
```

### Mix/package shape

- project module: `Membrane.VideoInterop.MixProject`;
- app/package: `:membrane_video_interop`;
- dependencies: `membrane_core` and `video_interop` only, plus documentation
  tooling in development;
- remove Rustler and the Rust CI/package job;
- ensure `mix hex.build` contains no Rust, NIF, lease, descriptor, format, or
  validator implementation.

### Adapter acceptance

Test:

- exact `:video_interop` metadata insertion and extraction;
- PTS/DTS and unrelated metadata preservation;
- rejection of nonempty payloads;
- rejection of `:dmabuf`, both metadata keys on one buffer, old structs/maps,
  and wrong values;
- `fetch_frame!/1` error shape;
- direct `%VideoInterop.Format{}` use in a small Membrane source-to-sink format
  negotiation test.

Do not copy `VideoInterop` lease-owner tests into the adapter; that behavior is
owned and tested by the generic package. Sink tests use fake consumers and real
leases only to prove ownership routing at the Membrane boundary.

## Phase 2: migrate Emerge headless output and the demo

The authoritative Emerge consumer, direct connection, and synchronous shutdown
design is in
[`library-owned-video-lifecycle.md`](library-owned-video-lifecycle.md). Emerge
is not a Membrane component and must depend directly on `video_interop`, not
`membrane_video_interop`.

### `/workspace/emerge-headless`

- Replace Mix `membrane_dmabuf` with `video_interop` and Cargo
  `membrane-dmabuf` with `video-interop`.
- Emit `%VideoInterop.Frame{storage:, acquire_sync:, lease:}` while retaining
  the storage-mode association key `"dmabuf"`.
- Consume canonical frames through `PreparedVideoFrame -> ClaimedVideoFrame`;
  applications never construct descriptor maps or handle keepalive messages.
- Implement the generic consumer protocol for `VideoTarget` and expose a
  consuming `EmergeSkia.submit_video_frame/2`.
- Add a direct headless-output-to-target connection and make normal
  `EmergeSkia.stop/1` await producer lease drainage and native stop.
- Apply the issue ownership rule exactly: release the private backend token only
  after `{:error, {:caller_owned, reason}}`, never after success or a
  `{:transferred, reason}` error.

### `/workspace/emerge_demo`

- Use the default Emerge renderer.
- Start the headless source disconnected, create the window `VideoTarget`, and
  connect them with `Emerge.connect_video_output/3`.
- Delete application descriptor conversion, `PrimeBridge`, `PrimeRenderer`,
  keepalive handling, and manual release logic.

The detailed native claim point, direct connection state machine, stop behavior,
and tests are defined in `library-owned-video-lifecycle.md`.

## Phase 3: migrate `membrane_video_transcode`

Update Mix dependencies to direct `video_interop` plus
`membrane_video_interop`. Update the decoder NIF from `membrane-dmabuf` to
`video-interop`.

For `output: :dmabuf`:

- emit `%VideoInterop.Format{storage: %VideoInterop.DMABuf.Format{}}`;
- construct `%VideoInterop.Frame{storage:, acquire_sync:, lease:}`;
- attach with `Membrane.VideoInterop.put_frame/2`;
- use `VideoInterop.LeaseOwner` and `:video_interop_*` notifications;
- preserve one isolated lease-owner mailbox per decoder/native pool;
- preserve native object sizes, modifiers, layers, planes, and borrowed fds.

Keep `output: :prime` only on the preserved rollback ref/artifact. Do not ship
it beside the canonical mode in the migration branch.

Audit every `LeaseOwner.issue/3` path under its frozen ownership rule:

- `{:caller_owned, reason}`: the producer still owns and releases the native
  token;
- after send (`:ok` or `{:transferred, reason}`): the lease owner owns the token;
- after successful issue, a later frame-build/send failure releases the public
  lease, not the native token directly.

Make decoder backend-token release idempotent and configure the generic
single-flight bounded-exponential retry policy. The decoder library owns
retry/exhaustion/destructor fallback and diagnostics; applications do not.
Tests must cover format negotiation, `metadata.video_interop`, fan-out holders,
mailbox isolation, issue rejection, retry success/exhaustion, flush/EOS, and
shutdown with displayed frames.

## Phase 4: migrate `membrane_libcamera`

Update:

```text
mix.exs / mix.lock
.cargo/config.toml
native/libcamera/Cargo.toml / Cargo.lock
lib/membrane_libcamera/source.ex
lib/membrane_libcamera/frame.ex
lib/membrane_libcamera/native.ex
native/libcamera/src/frame.rs
native/libcamera/src/lib.rs
README and tests
```

### Public output

Keep `output: :dmabuf`, but emit:

- `%VideoInterop.Format{storage: %VideoInterop.DMABuf.Format{}}`;
- `%VideoInterop.Frame{storage:, acquire_sync:, lease:}`;
- `metadata.video_interop` through `Membrane.VideoInterop.put_frame/2`;
- generic lease notifications.

Keep legacy `output: :drm_prime` only on the preserved rollback ref/artifact.
Canonical output must not be implemented by relabeling a public
`%Membrane.PrimeDesc{}` or shipping both protocols.

Apply the same issue-ownership audit explicitly to single-frame, analysis,
frame-set, capacity, draining, and send/build failure branches. Current
libcamera source branches directly release native tokens after issue errors;
remove those post-issue releases while preserving direct cleanup only before an
issue request is sent.

Preserve:

- object size and modifier metadata;
- object/plane index, offset, and pitch relationships;
- root plus distinct child holders for dual-stream output;
- isolated release callbacks and shutdown draining;
- strict NV12 and visible-rectangle validation.

### Native diagnostics

Replace Rust `VideoFrame`/`Synchronization` field access with
`video_interop::Frame`, `Storage`, and `AcquireSync`.

- Synchronous probes may call `prepare_cloexec()`, inspect
  `PreparedVideoFrame.frame()`, and drop it unclaimed; Elixir still owns release.
- Delete unused retained-frame diagnostic APIs where possible.
- Delete the currently unused `retain_video_frame` diagnostic if it has no
  caller. If an asynchronous retained native resource remains, it must call
  `claim()` only after admission succeeds, store `ClaimedLease`, and retire it
  at exact hardware completion. It must not return an error after claim.
- Reject unsupported acquire fences before asynchronous admission until the
  consumer waits on them correctly.

Update `MembraneLibcamera.Frame` thumbnail extraction to retain a unique child
lease, perform bounded synchronous work, and release the child in `after`.

Make libcamera backend-token release idempotent and configure the generic
single-flight bounded-exponential retry policy. The plugin owns
retry/exhaustion/destructor fallback and diagnostics; applications do not.
Tests must include issue-error ownership, retry success/exhaustion, dual-stream
fan-out, synchronous unclaimed probes, any remaining claimed diagnostics, stale
sessions, shutdown drain, and repeated frames without fd growth. Retain the
hardware 1,000+ frame release test.

## Phase 5: cut over the downstream camera application

The camera worktree is dirty and currently points to `/workspace/emerge`, while
the generic headless work is in `/workspace/emerge-headless`. Resolve and pin
that branch/worktree mismatch before hardware validation.

Update direct dependencies to `video_interop` and
`membrane_video_interop`. Keep `output: :dmabuf` configuration.

Replace `Camera.VideoSink` display-lifecycle code with
`%Membrane.VideoInterop.Sink{consumer: video_target}`. Move synchronous
analysis/probe cleanup into reusable library/plugin consumers that return
results to application policy code; application modules must not parse frame
storage, release holders, handle lease atoms, or construct native submission
maps.

For retained diagnostic resources, migrate the NIF and its plugin-owned
consumer as one API change. Native code claims only after admission and owns
exact retirement. Synchronous probes run inside a library helper that releases
in `after`; camera application callbacks receive copied results rather than a
borrowed frame holder.

The generic sink/session layer enforces the submission rule: caller-owned
validation/admission failures are released once, transferred frames retire in
the consumer, and no per-frame fallback to the old contract occurs.

Update Nerves staging and lockfiles, then verify the Rust 1.91/edition 2024
requirement against the RPi5 toolchain.

## Phase 6: remove compatibility and stale terminology

After host and hardware soak:

1. Remove old `membrane_dmabuf` dependency paths and Cargo patches.
2. Remove `%Membrane.DMABuf.*{}`, `metadata.dmabuf`, and
   `:membrane_dmabuf_*` source references.
3. Remove legacy canonical helper names such as `submit_dmabuf` only where they
   mean the old transport contract; retain `:dmabuf` where it accurately names
   storage or diagnostics.
4. Keep old `:prime`/`:drm_prime` rollback only on preserved refs/artifacts;
   canonical deployment branches must not ship both lease protocols.
5. Update historical active plans and docs without rewriting archived history.
6. Archive or remove the old `/workspace/colibri/membrane_dmabuf` checkout only
   after every migrated path and cold-rollback artifact has been verified.

Search gate outside archived plans:

```text
Membrane.DMABuf
membrane_dmabuf
membrane-dmabuf
:membrane_dmabuf_
metadata.dmabuf
%{dmabuf:
[:dmabuf]
Map.get(..., :dmabuf)
Map.fetch(..., :dmabuf)
Map.put(..., :dmabuf)
```

Use syntax-aware or separate literal searches rather than treating the ellipses
above as one regular expression. Expected remaining `dmabuf` text must refer to Linux storage, DRM/GBM/EGL
operations, output modes, or diagnostics—not the removed public contract.

## Validation matrix

### `video_interop`

```bash
mix format --check-formatted
mix test
mix hex.build
mix docs
cargo fmt --all -- --check
cargo test --workspace
cargo test -p video-interop --no-default-features
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p video-interop --no-default-features --all-targets -- -D warnings
cargo package -p video-interop --allow-dirty
```

### Adapter

```bash
mix format --check-formatted
mix test
mix docs
mix hex.build
```

Assert the Hex archive contains only Membrane adapter code and dependencies.

### Consumers

- full Emerge default/raster-only/DRM tests and Clippy matrix;
- demo PRIME validation tests;
- `membrane_video_transcode` Mix and native Rust suites;
- `membrane_libcamera` host/mock/native suites;
- camera host tests and RPi5 cross-build;
- `git diff --check` and lockfile/package resolution in every repository.

### Hardware acceptance

At the production camera configuration, require:

- expected camera production and presentation rates;
- bounded camera/decoder/export pools;
- no request or slot reuse before final holder retirement;
- no lease-owner mailbox growth;
- no fd growth over at least 10,000 frames;
- correct fan-out retirement for preview and analysis streams;
- clean renderer/source shutdown and repeated pipeline restarts;
- unchanged image geometry and orientation.

## Commit and publication order

Recommended commit slices:

1. Commit/tag generic `video_interop` foundation.
2. Commit/tag the standalone `membrane_video_interop` adapter.
3. Migrate Emerge headless output.
4. Migrate demo bridge.
5. Migrate video-surfaces producer.
6. Migrate libcamera producer and diagnostics.
7. Migrate camera application.
8. Remove compatibility paths and update docs.

Publish only after lockstep validation:

1. crates.io `video-interop`;
2. Hex `video_interop`;
3. Hex `membrane_video_interop`;
4. Emerge/precompiled NIF release;
5. `membrane_video_transcode`;
6. `membrane_libcamera`;
7. downstream camera firmware/application.

No published artifact may contain a sibling path dependency or
`[patch.crates-io]` override. Never publish `membrane_dmabuf`.

Publishing does not make a mixed runtime safe. Deployment and rollback require
one of:

- a verified full drain of every old holder before code replacement; or
- the normal choice for this migration: a cold BEAM/firmware restart with the
  complete producer/consumer/application closure on one contract version.

Do not use a rolling hot upgrade and do not downgrade only one side of the
lease protocol. A cold rollback must boot the recorded old artifact manifest;
it must not reuse in-memory frames or lease owners from the failed version.

## Definition of done

- One generic Hex package and Rust crate own frame, format, synchronization,
  validation, fd ownership, and lease semantics.
- `membrane_video_interop` only integrates `VideoInterop.Frame` with
  `Membrane.Buffer`.
- Membrane stream formats use `VideoInterop.Format` directly.
- Producers emit one `metadata.video_interop` frame with one holder per branch.
- Native and Elixir consumers release exactly once at proven retirement.
- Emerge remains Membrane-independent and generic PRIME input remains
  producer-independent.
- Host, package, cross-build, and sustained hardware gates pass without fd,
  mailbox, latency, orientation, or restart regressions.
