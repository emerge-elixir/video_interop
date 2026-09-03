# VideoInterop audit remediation plan

Status: host implementation complete; pinned-RPi5 qualification remains.

Implemented host scope includes reservation-based issue admission, serial release execution,
non-raising lifecycle calls, dispatcher delivery accounting, exact fd size proof, shader address
bounds, unique import identity/state enforcement, renderer-owned queue submission, immutable NV12
candidate resolution, direct NV12 caching, schema tightening, reproducible shaders, CI coverage,
and Vulkan capability/error/identity/synchronization/test module splits. Emerge and MembraneLibcamera integrations are migrated and their
host suites pass. Hardware-only validation gates below remain open.

This plan resolves every finding in `video-interop-audit.md`. The work is intentionally split into
small correctness-preserving batches. The pinned RPi5 transfer path must remain
`LinearBufferToOptimalYuvPlanes`; `auto` must not gain an allocation-triggered compute fallback.

## Non-negotiable invariants

- Preserve prepare -> claim -> exact retirement and caller/transferred ownership receipts.
- Never destroy, reuse, or return an imported source/output while GPU work may reference it.
- Keep explicit `SYNC_FD`, `VK_QUEUE_FAMILY_EXTERNAL`, exact DRM-device identity, and terminal
  device quarantine on uncertain Vulkan state.
- Bind the complete truthful DMA-BUF allocation, but address/copy only validated logical spans.
- Keep all caches and pools bounded, stream-incarnation-aware, and idle-evicted.
- Keep forced NV12 modes fail-closed. Strategy selection occurs once before frame admission and is
  immutable for the stream.
- Do not add CPU upload, EGL/GL interop to Vulkan, software Vulkan, or silent renderer fallback.
- Keep release callbacks off the lease-owner mailbox and blocking dispatcher joins on dirty-I/O
  schedulers or native threads.

## Affected repositories

Primary implementation:

- `/workspace/video_interop`

Required integration changes:

- `/workspace/emerge-headless`: Vulkan queue authority, owning import transaction, immutable NV12
  recipe, typed error/statistics handling, and updated lifecycle result handling.
- `/workspace/colibri/membrane_libcamera`: two-stage issue-result tests and defensive
  `LeaseOwner.stats/2`/`close/2` handling.
- `/workspace/membrane_video_interop`: lifecycle contract tests if public result types surface.
- `/workspace/colibri/camera`: qualification only unless a changed result shape reaches diagnostics.

No libcamera allocation or Nerves system change is expected.

## Target API decisions

### Typed Vulkan errors

Add a public non-exhaustive `VulkanImportError` using `thiserror`, with structured variants at least
for:

- invalid dimensions/layout/topology/conversion;
- declared versus observed allocation-size mismatch;
- Vulkan memory-requirement mismatch;
- unsupported capability/strategy;
- cache identity collision and active reuse;
- source-cache and output-pool saturation;
- lock poisoning;
- Vulkan operation failure with operation name and `vk::Result`;
- queue-thread violation;
- invalid synchronization transition/import mismatch;
- device lost/quarantined.

Use subordinate enums such as `PoolKind`, `CacheKind`, and `LayoutErrorKind`. Keep `Display` text for
logs, but use variants for statistics and policy. `ImportedImageSyncError` may remain a focused
public type; it must convert to/from the shared device/queue error without string parsing.

During migration, an internal `Context(String)` variant may bridge renderer-specific diagnostics.
It must not be used for conditions that drive counters or fallback policy.

### Immutable NV12 resolution

Capability inventory returns direct candidates plus the highest-priority independently proven
staged candidate, including more than one candidate for modifier zero when direct and staged are
both advertised. Auto keeps transfer ahead of compute during inventory, so a proven transfer path
does not initialize or depend on a compute fallback. Add:

```rust
pub struct Nv12ResolveRequest {
    pub modifier: u64,
    pub dimensions: (u32, u32),
    pub conversion: Nv12Conversion,
}

impl<D: VulkanDeviceContext> VulkanDmaBufImporter<D> {
    pub fn resolve_nv12(
        &self,
        request: Nv12ResolveRequest,
    ) -> Result<Nv12ModifierCapability, VulkanImportError>;
}
```

The importer stores its construction-time `Nv12StagingPreference`. Resolution filters every
candidate against exact extent, external-memory, filtering, chroma-siting, transfer, and pipeline
requirements, then selects by policy order:

- `PreferPlanar`: non-linear image-to-optimal Y/UV transfer or linear buffer-to-optimal Y/UV
  transfer, followed by compute Y/UV and compute RGBA fallbacks in capability-discovery order.
- `RequirePlanar`: compute Y/UV only.
- `RequireRgba`: compute RGBA only.

A stream stores the returned capability and attests its exact allocation recipe. It never resolves
again per frame and never changes strategy because import/allocation later failed. On the pinned
V3DV device, resolution must remain `LinearBufferToOptimalYuvPlanes`.

### Owning Vulkan import transaction

Give every importer claim a monotonic nonzero `ImportId`. Replace the loose pairing of
`ImportedDmaBufImage` and `ImportedImageSync` with an owning transaction whose states cannot be
mixed across images:

```text
Prepared
  -> AcquireSubmitted
  -> RendererAccepted
  -> ReleaseSubmitted
  -> Complete/Recyclable

any uncertain Vulkan result -> Quarantined
```

Prefer consuming transition methods (separate state types or one private enum behind an owning
public transaction). The transaction owns the imported image, staged source/output leases, sync
lane, acquire/release fences, command pools/buffers, queries, and any semaphore still owned by
VideoInterop.

Required behavior:

- `submit_acquire` records the exact import id and returns one one-shot ready semaphore.
- `ganesh_wait_accepted` verifies that exact semaphore and transfers its destruction ownership only
  once.
- `source_release_complete()` takes no image argument; it can release only the transaction's staged
  source.
- `submit_release` is unavailable/rejected before renderer acceptance and cannot accept another
  image.
- only a proven release fence returns a sync lane and source/output claims to pools.
- dropping `Prepared` performs normal RAII cleanup.
- dropping any submitted/in-flight state marks the context quarantined, skips individual Vulkan
  destruction for possibly live raw handles, and leaves cleanup to final `vkDestroyDevice`.
  Emerge must stop use of that device immediately. This is a terminal leak-until-device-teardown,
  not a recoverable pool leak.
- low-level raw-handle escape hatches are explicitly `unsafe` and document queue, lifetime, and
  destruction preconditions.

Emerge's existing imported-image ticket becomes the owner of this transaction. Its separate
`sync`/`allocation` options are removed so incorrect pairing is structurally impossible.

### Queue authority

Do not call `vkQueueSubmit` through a raw queue accessor in VideoInterop. Extend
`VulkanDeviceContext` with a context-owned submission operation and a terminal quarantine hook.
The implementation must either:

1. assert one renderer thread owns all queue host access, including Ganesh, or
2. use one gate shared by VideoInterop and every Ganesh queue interaction.

Emerge will use option 1 initially: record the Vulkan renderer thread at context construction,
reject another thread with a typed error, and preserve the current same-thread Ganesh sequence.
Tests must prove two independently created sync lanes cannot bypass the authority.

### Bounded issue reservation

Change `LeaseOwner.issue/3` to reserve capacity before sending the backend token:

```text
caller -- reserve(metadata, alias) --> owner
caller <-- reserved(reservation) ----- owner
caller -- commit(reservation, backend_token, alias) --> owner
caller <-- lease/error ---------------- owner
```

Rules:

- Reserve messages never contain the backend token. Draining/capacity/owner-down/reservation timeout
  therefore return `{:error, {:caller_owned, reason}}`.
- The token-bearing commit send is the sole ownership boundary. Every later timeout/error is
  `:transferred`.
- Reservations count against `max_active`, are monitored, and are cancelled on caller death or
  timeout.
- Reservations accepted before drain may commit; drain waits for them or their ordered
  cancellation. New reservations after drain are rejected.
- Commit atomically replaces one reservation with one lease entry, so active, releasing, and failed
  entries plus reservations never exceed finite `max_active`.
- Guard-construction failure after commit follows normal asynchronous release and retains the same
  bounded slot until successful release.
- `:infinity` remains an explicit unbounded configuration; finite bounds are never bypassed.
- Backend tokens still require an independent owner-crash/message-drop destructor for the race
  between token-bearing send and owner death.

The public success/error shape remains unchanged, but capacity/draining failures become
caller-owned because transfer no longer occurred. Update every producer branch and ownership test.

### Asynchronous release executor

Move arbitrary release callbacks out of the `LeaseOwner` callback process. Start one monitored,
serial executor per owner. The owner enqueues `{token, attempt, generation, backend_token}` and
marks the entry `{:releasing, ...}`. The executor catches exception/throw/exit exactly as today and
returns result plus timing.

Rules:

- one callback at a time provides deterministic ordering and bounded concurrency;
- one generation per token rejects stale results;
- `retry/3` returns `:release_in_progress` while a generation is active;
- success removes the lease and may complete drainage;
- failure stores the exact token/reason and follows existing manual/exponential single-flight retry;
- a worker crash marks its in-flight generation failed with an uncertainty diagnostic, starts a new
  executor, and retries only under the configured idempotent policy;
- no timeout kills a callback: completion uncertainty is not recoverable without the backend's
  idempotency contract;
- owner shutdown waits for no reservations, holders, releasing entries, failed entries, or executor
  work before stopping the executor;
- expose executor queue depth, active callback age, and worker restarts in stats.

This keeps the owner mailbox responsive even if a callback stalls while preserving correctness by
allowing drain to remain pending.

### Public owner lifecycle behavior

- Preserve `start_link/1`'s deliberate ordinary producer link: an OTP parent link would terminate
  the owner with its producer before outstanding leases could drain. Document this nonstandard but
  correctness-bearing behavior explicitly.
- Add `start_supervised/1` and a temporary-worker `child_spec/1` for a supervisor-owned process that
  monitors a distinct producer while retaining normal OTP parent shutdown behavior.
- Route `close/2` and `stats/2` through the same non-raising call wrapper as `retry/3`.
  `stats/2` returns the stats map on success and `{:error, :timeout | {:owner_down, term()}}` on
  failure; `close/2` adds the same error alternatives.
- Make `Lease.retain/2` local-owner-only, preflight and monitor the owner, clean aliases/monitors on
  every path, and return `{:owner_down, reason}` immediately after owner death.
- Update downstream callers to handle errors instead of `catch` around `GenServer.call` exits.

### Dispatcher delivery failures

A failed `OwnedEnv::send_and_clear` to a dead local lease owner is not worker corruption: the
producer/owner-crash destructor is then authoritative. Add `DispatchOutcome::Delivered |
OwnerUnavailable`, increment an atomic undelivered counter, and keep the worker healthy. Expose the
counter through `DispatcherProbe` and final close diagnostics.

Channel loss, worker panic, client underflow, or lifecycle-owner drop before join remain fatal.
Tests must distinguish dead-recipient delivery from actual dispatcher corruption.

## Implementation phases

### Phase 0 — Freeze baseline and add CI gates

1. Add `scripts/check-vulkan-shaders.sh` and `scripts/regenerate-vulkan-shaders.sh`.
2. Pin glslang and SPIRV-Tools versions plus download checksums. Record the intended target
   environment for each shader; regenerate all three artifacts once with the pinned compiler.
3. `--check` compiles to a temporary directory, byte-compares all `.spv` files, and runs
   `spirv-val`. It must not rewrite the worktree.
4. Extend CI with:
   - `cargo test --workspace --no-default-features --features vulkan`;
   - all-target/all-feature warnings-denied Clippy;
   - all-feature tests;
   - shader check;
   - `cargo package` followed by Vulkan-feature build/test from the crate archive.
5. Preserve current default/core and EGL jobs.

Exit gate: CI detects a one-byte GLSL/SPIR-V mismatch and the unmodified tree passes.

### Phase 1 — Typed error foundation and module-independent regression tests

1. Add `vulkan/error.rs` and convert public importer/capability/layout results from `String` to
   `VulkanImportError`.
2. Replace every `starts_with` statistics branch with variant matching.
3. Keep downstream log wording through `Display`; update Emerge conversions to retain error kinds
   before rendering strings.
4. Add pure state/layout tests for all new variants before changing behavior.

Exit gate: no policy/statistics code parses error text; existing success strategies are unchanged.

### Phase 2 — Allocation truth, compute bounds, and construction RAII

1. Add `DmaBufIdentity { device, inode, allocation_size }`.
2. Query Linux DMA-BUF size using the kernel-supported `SEEK_END` probe, restoring the original
   position when meaningful; cross-check nonzero `fstat.st_size`. Reject unavailable, disagreeing,
   under-reported, and over-reported sizes before cache lookup/import.
3. Apply this to direct/staged NV12, packed input, and scanout external-memory imports.
4. Include observed size in cache keys/collision diagnostics.
5. Add a shared checked NV12 visible-span helper. Compute staging requires all shader byte
   expressions to fit `u32` and the exclusive visible span to be at most `u32::MAX + 1`.
6. Bind full allocation size but create the uniform texel-buffer/view over only the aligned logical
   visible span. Transfer copy regions remain unchanged and exclude the 256-byte tail.
7. Add scoped Vulkan handle guards for create/reset/import/bind sequences. Fix the ready-semaphore
   reset failure first, then audit all sibling constructors.

Tests:

- memfd exact/short/long/zero/nonseekable size cases and file-position restoration;
- 2560x1440 allocation `5_529_856`, logical span `5_529_600`;
- maximum accepted and first rejected compute address;
- overflow in offset + row*pitch + width;
- injected constructor failure proves each handle is destroyed exactly once.

Exit gate: no external-memory import trusts descriptor size alone and no shader path can wrap a byte
address.

### Phase 3 — Queue authority and owning synchronization transaction

1. Add `ImportId`, context queue submission, thread/gate enforcement, and quarantine hook.
2. Implement the transaction state machine and move staged-source release inside it.
3. Make handle construction and ownership transfer transactional, including ready semaphore
   handoff to Ganesh.
4. Remove or make unsafe the old loose methods accepting arbitrary images.
5. Migrate Emerge's Vulkan import ticket and sync pool atomically in the same integration batch.
6. On any device-lost/uncertain state, quarantine the transaction, stop imports, and terminate the
   active Vulkan renderer/device epoch; do not recycle source/output/sync slots.

Tests:

- every legal and illegal pure state transition;
- image A cannot poll/release image B;
- wrong/stale ready semaphore rejected;
- release-before-Ganesh rejected without queue submission;
- second-thread queue submit rejected;
- dropped prepared transaction cleans normally;
- dropped in-flight transaction marks quarantine and performs no child destruction;
- release fence completion is the only lane/output reuse authority;
- Emerge replacement, stream close, context loss, and renderer shutdown retain exact retirement.

Target gate: Vulkan validation enabled, no VUID/MMU errors under normal, replacement, shutdown, and
injected-sync-failure runs.

### Phase 4 — Candidate resolver and complete NV12 source caching

1. Inventory all valid candidates instead of suppressing staging when direct import is advertised.
2. Resolve once with exact stream conversion/dimensions and store the chosen capability in Emerge's
   consumer-session/target admission state.
3. Require every frame to match that immutable capability and allocation recipe.
4. Extend `CachedNv12SourceAllocation` with a direct-image variant and route direct NV12 through the
   same stream/device/inode/size/topology/strategy cache.
5. Preserve active-claim rejection, idle-only LRU eviction, stream incarnation eviction, and bounded
   cache limits for direct and staged entries.
6. Extend statistics with per-strategy resolutions, direct cache hit/miss/eviction, and resolver
   rejection reasons.

Tests:

- direct candidate fails chroma siting while transfer candidate resolves;
- forced modes never select another strategy;
- no candidate returns one aggregate typed diagnostic;
- selection is immutable after a later allocation/import failure;
- direct cache hit, active reuse rejection, topology collision, idle eviction, and stream eviction;
- pinned synthetic V3DV capabilities resolve to `LinearBufferToOptimalYuvPlanes`.

Target gate: runtime proof still reports `object_size=5529856` and
`strategy=LinearBufferToOptimalYuvPlanes`; no per-frame source import/allocation churn.

### Phase 5 — Dispatcher outcome accounting

1. Add delivery outcome/counters without changing FIFO or client pinning.
2. Keep actual worker/channel/lifecycle corruption fatal.
3. Expose delivered/undelivered counts in probe/final diagnostics and downstream shutdown logs.
4. Add Rust and ExUnit test-NIF coverage for live owner, dead owner, delayed FIFO drain, worker panic,
   and exact close/join.

Exit gate: dead-recipient release is visible and delegated to owner-crash fallback; it neither
silently disappears nor marks a healthy worker failed.

### Phase 6 — Bounded reservation protocol and release executor

Implement reservation first, then executor, so asynchronous rejection failures can never bypass
capacity.

Tests must cover:

- capacity/draining rejection remains caller-owned and invokes no release callback;
- timeout before commit is caller-owned; timeout after commit is transferred;
- owner death before/after commit boundary;
- reservation caller death/cancellation and drain ordering;
- concurrent reservations never exceed finite `max_active`;
- guard-factory failure and failed release remain within the bound;
- callback blocking does not delay `stats`, retain rejection, drain waiter registration, or producer
  exit handling;
- one callback per token, stale generation ignored, worker crash, manual retry, exponential retry,
  exhaustion, and final drainage;
- mailbox, executor queue, retained token count, timers, and process count remain bounded in a flood
  test.

Update MembraneLibcamera issue ownership tests and producer release branches in the same batch.

Exit gate: finite `max_active` bounds reservations + active + releasing + failed tokens under every
release result.

### Phase 7 — Lifecycle API, schema, statistics, and documentation edges

1. Correct `start_link`, non-raising close/stats, and monitored retain behavior.
2. Maintain `active_holders` incrementally.
3. Maintain an ordered `:gb_sets` index of `{issued_at_ns, token}` for O(log n) oldest-age lookup;
   remove only after successful final release. Invariant tests compare counters/index against the
   lease map after randomized operations.
4. In both Elixir and Rust descriptor validators, collect referenced object indices across every
   layer and reject unreferenced objects before FD duplication. Sharing one object across planes or
   layers remains valid.
5. Update README allocation example to `5_529_856` and explicitly identify the `5_529_600` visible
   span and untouched 256-byte tail.
6. Update changelogs, Rust README, ownership tables, callback/executor contract, issue boundary,
   stats result types, and downstream architecture guides.

Exit gate: Elixir and Rust schemas reject the same descriptors, lifecycle calls do not unexpectedly
exit, and docs describe the actual pinned allocation.

### Phase 8 — Vulkan module split without semantic change

Move code in reviewable, behavior-neutral commits behind stable re-exports:

```text
vulkan/
  mod.rs
  error.rs
  types.rs
  capability.rs
  layout.rs
  identity.rs
  allocation.rs
  cache.rs
  importer.rs
  staging/
    mod.rs
    compute.rs
    transfer.rs
  sync.rs
```

Move tests with their owning modules and retain black-box integration tests for public APIs. Do not
combine a code move with behavior changes. `mod.rs` should contain documentation and re-exports,
not implementation policy.

Exit gate: public paths remain available through `video_interop::vulkan::*`, generated docs contain
all public contracts, and before/after test and target statistics match.

## Validation matrix

Run after every primary-repository phase:

```bash
cd /workspace/video_interop
cargo fmt --all -- --check
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
mix format --check-formatted
mix test
mix hex.build
```

Run after cross-repository API phases:

```bash
cd /workspace/emerge-headless
VIDEO_INTEROP_PATH=../video_interop mix deps.get
VIDEO_INTEROP_PATH=../video_interop cargo test --manifest-path native/emerge_skia/Cargo.toml
VIDEO_INTEROP_PATH=../video_interop mix test

cd /workspace/colibri/membrane_libcamera
cargo test --workspace
mix test

cd /workspace/membrane_video_interop
mix test
```

Use each repository's warnings-denied Clippy/full CI command in addition to the abbreviated commands
above.

Pinned RPi5 qualification after Phases 2, 3, 4, and final:

- exact-pixel NV12 fixtures and live Camera Focus scene;
- Vulkan validation and V3DV MMU/kernel logs;
- acquire/release fault injection, replacement, hotplug, stream restart, and renderer restart;
- 10,000-frame and long-duration soak with stable FD/RSS/cache/pool/queue counts;
- deterministic shutdown and cold-boot OpenGL rollback;
- idle and active-slider throughput/latency capture;
- 60 FPS with at least 30% GPU headroom acceptance gate.

## Commit and rollback sequence

Use one commit per numbered phase in `video_interop`, with corresponding Emerge/Membrane integration
commits immediately after the API-changing phase. Never leave a downstream repository pointing at
an incompatible intermediate API for firmware construction.

Before each target deployment, record source heads, crate/NIF hashes, firmware hash, shader hashes,
and selected strategy in a durable manifest outside `_build`.

If a phase regresses the pinned target, revert that phase rather than silently changing strategy.
The explicit safe runtime rollback remains `EMERGE_VULKAN_NV12_STAGING=planar`, selecting
`LinearBufferToYuvPlanes`; OpenGL remains a cold-boot peer rather than an in-process Vulkan fallback.

## Completion criteria

All audit findings are complete only when:

- every finding has a regression test tied to its typed condition/state transition;
- no Vulkan policy depends on error text;
- exact DMA-BUF size and 32-bit shader addressing are proven before import;
- queue access and image/sync identity are structurally enforced;
- finite lease capacity cannot be bypassed and callbacks cannot stall the owner mailbox;
- dead dispatcher recipients are visible and delegated to the documented fallback;
- direct NV12 imports are persistent and bounded;
- CI exercises Vulkan and reproducible validated shaders;
- all host/cross-repository suites and pinned-target qualification pass without changing the
  selected production strategy.
