# VideoInterop bottom-up audit

Status: host fixes implemented from baseline `3ac3583`; pinned-RPi5 qualification remains.

## Scope

- Elixir frame/format/descriptor validation and consumer contracts.
- `LeaseOwner` issue, retain, release, retry, abandonment, and drainage.
- Rust descriptor duplication and Rustler resource lifecycle.
- Optional EGL synchronization.
- Vulkan DMA-BUF capability selection, persistent imports, staging, explicit synchronization,
  pooling, and device-loss behavior.
- Tests, shaders, packaging, and CI.

## Baseline

The current tree passes:

- `cargo test --workspace --all-features` (37 unit, 9 integration/schema tests);
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`;
- `mix test` (97 tests);
- `mix format --check-formatted` and `git diff --check`.

No block-severity Rustler resource-registration, `Vec<u8>` binary, normal-scheduler production
NIF, or BEAM-thread `send_and_clear` defect was found. FD duplication is CLOEXEC and partial
failure cleanup is sound. EGL keeps KHR/core ABIs separate and handles EGL FD transfer semantics
explicitly. The Vulkan source caches and output pools are bounded and reject active reuse.

## Findings

### P1 — Bind synchronization lanes to one imported image and enforce their state machine

`ImportedImageSync` stores only booleans. Its safe public methods accept an arbitrary
`ImportedDmaBufImage` each time. A caller can acquire image A, poll source completion using image B,
and submit release for image C. `submit_release` also permits release before acquire and before the
renderer accepts the ready semaphore. `Drop` destroys command pools, fences, queries, and owned
semaphores without proving submitted work is idle.

This is correct in Emerge's current single-owner call sequence, but the generic safe API does not
enforce that sequence and misuse can release a producer source early or destroy in-flight Vulkan
objects.

Fix:

1. Give every imported claim a unique `ImportId`.
2. Replace booleans with an explicit state enum: `Idle -> AcquireSubmitted -> RendererAccepted ->
   ReleaseSubmitted -> Complete`, plus `Quarantined`.
3. Store the import id and staged/direct facts at acquire. Remove the image argument from
   `source_release_complete`; verify the same id in release.
4. Reject release before acquire/renderer acceptance.
5. Return an owning transaction/retirement object that keeps image and sync resources together.
6. On uncertain drop, transfer all children to caller-owned quarantine or intentionally retain them
   until device teardown; never destroy potentially in-flight objects.

Relevant code: `rust/video-interop/src/vulkan/sync.rs:198-533`.

### P1 — Represent Vulkan queue external synchronization in `VulkanDeviceContext`

The context is `Send + Sync`, exposes a raw queue, and each sync lane directly calls
`vkQueueSubmit`. Vulkan requires host access to one queue to be externally synchronized. Separate
lanes can currently submit concurrently through the safe Rust API.

Fix:

- Replace raw queue submission in the adapter with a context operation that serializes or asserts
  render-thread ownership for every submit. The same authority must cover Ganesh submissions to the
  queue, not only VideoInterop calls.
- Document and test the queue-thread/locking contract.

Relevant code: `rust/video-interop/src/vulkan/mod.rs:122-135`,
`rust/video-interop/src/vulkan/sync.rs:258-262,394-398`.

### P1 — Verify the published DMA-BUF allocation size against the FD

Vulkan import compares memory requirements with caller-provided `source_size`/`object_size`, but it
never verifies that value against the DMA-BUF itself. An over-reported descriptor can therefore pass
library checks before being handed to external-memory import.

Fix:

- Extend the existing `fstat` identity query to obtain `st_size` and require exact equality with the
  published object size before cache lookup or any Vulkan allocation/import.
- Include the observed size in typed diagnostics and cache collision evidence.
- Add tests for exact match, under-report, over-report, and non-DMA-BUF/zero-size descriptors.

Relevant code: `rust/video-interop/src/vulkan/mod.rs:2844-2858,3445-3645`.

### P1 — Bound compute-planar NV12 addressing to the shader's 32-bit byte domain

Packed staging rejects allocations above 2^32 bytes. NV12 compute staging only checks four-byte
alignment and `maxTexelBufferElements`, while its GLSL computes plane byte addresses in `uint`.
A valid-looking layout above the 32-bit byte range can wrap shader addresses.

Fix:

- Compute the final exclusive Y/UV span from dimensions, offsets, and pitches and require it to be
  at most `u32::MAX + 1`.
- Use that four-byte-aligned visible span as the logical uniform-texel buffer/view range while
  retaining the truthful full allocation for external memory import.
- Validate push-constant conversions before importing/caching source memory.

Relevant code: `rust/video-interop/src/vulkan/mod.rs:2765-2788,1562-1591` and
`rust/video-interop/src/vulkan/nv12*.comp`.

### P1 — Put the Vulkan feature and shader artifacts under CI

CI tests default/core and EGL features but never compiles, tests, or lints `--features vulkan`.
The committed `.comp.spv` files are only checked for a SPIR-V magic word and minimum length; editing
GLSL without regenerating SPIR-V still passes.

Fix:

- Add no-default Vulkan test and warnings-denied Clippy jobs, plus an all-features build.
- Pin a shader compiler/tool version, regenerate each `.spv`, byte-compare it with the committed
  artifact, and run `spirv-val`.
- Package-test the Vulkan feature from the generated crate archive.

Relevant code: `.github/workflows/ci.yml:29-36` and shader helpers/tests in
`rust/video-interop/src/vulkan/mod.rs`.

### P1 — Clean up the ready semaphore if acquire-fence reset fails

`submit_acquire` creates `ready`, then uses `?` on `reset_fences`. That error path neither stores nor
destroys `ready`, leaking one semaphore per failure.

Fix: use a small RAII handle guard for newly created Vulkan objects and disarm it only after
ownership is stored/transferred. Apply the same pattern throughout multi-step Vulkan construction.

Relevant code: `rust/video-interop/src/vulkan/sync.rs:225-241`.

### P2 — Resolve NV12 strategy per conversion instead of suppressing staged linear candidates

Capability inventory adds linear staging only when no direct linear capability exists. A driver can
advertise importable direct linear NV12 while lacking the exact filtering/chroma-siting features for
a stream. In that case direct validation fails later, but the valid transfer candidate was never
published.

Fix:

- Inventory candidate recipes rather than one capability per modifier.
- Add an importer-owned resolver accepting modifier, dimensions, conversion, and staging policy.
- Return one immutable selected capability/recipe for stream attestation and use that exact value
  for every frame.
- Keep forced modes fail-closed and do not introduce an unrequested runtime strategy switch.

Relevant code: `rust/video-interop/src/vulkan/mod.rs:3940-3982`.

### P2 — Treat release-dispatch send failure according to the documented terminal policy

The dispatcher worker discards `OwnedEnv::send_and_clear` errors even though documentation says
post-publication queue loss is terminal. This makes worker health look healthy after a release
message was not delivered.

Fix: classify send errors. Either make owner death an explicit terminal/fallback outcome with a
counter, or set `FAILED` and invoke the existing fatal corruption policy. Do not silently ignore it.
Add a dead-owner test in a subprocess.

Relevant code: `rust/video-interop/src/beam.rs:606-650`.

### P2 — Restore bounded producer ownership when rejected release callbacks fail

At capacity or during draining, every new issue is already transferred and immediately released.
If that callback fails, `release_entry` inserts a new failed entry into `leases`, bypassing
`max_active`. Repeated/concurrent issues can therefore grow retained backend tokens and retry timers
without bound.

Fix options, in preferred order:

1. Add a capacity-reservation handshake before transferring the backend token.
2. Bound reservations and failed-release entries explicitly.
3. If the bound is exceeded, enter a documented terminal owner state that relies on each token's
   independent owner-crash destructor; never silently discard a token.

Relevant code: `lib/video_interop/lease_owner.ex:733-760,822-867,1057-1058`.

### P2 — Keep arbitrary release callbacks off the lease-owner mailbox

The callback runs synchronously inside the `GenServer`. A blocking callback delays releases,
retains, drain requests, retries, and owner-exit handling despite the dedicated mailbox.

Fix: either make the callback's constant-time/nonblocking requirement explicit and enforce target
telemetry thresholds, or run one monitored release task per token with an explicit `:releasing`
state and generation. Preserve single-flight retry and exact drain semantics.

Relevant code: `lib/video_interop/lease_owner.ex:822-829,998-1018`.

### P2 — Normalize public owner-death/timeout behavior

`retry/3` and `drain/2` return errors, while `close/2` and `stats/2` call `GenServer.call` directly
and can exit their callers despite narrower specs. `Lease.retain/2` does not monitor its owner and
waits for the full timeout after owner death.

Fix:

- Use one monitored/alias request helper for public lifecycle calls that promise tuple errors.
- Update specs consistently.
- Monitor the lease owner during retain and return `{:owner_down, reason}` promptly.
- Preserve and explicitly document the ordinary producer link: replacing it with an OTP parent
  link prevents the owner from outliving producer exit to drain leases. Add an explicitly
  supervisor-safe API that monitors a distinct producer.

Relevant code: `lib/video_interop/lease_owner.ex:95-98,120-174` and
`lib/video_interop/lease.ex`.

### P3 — Cache direct NV12 imports

Packed direct images and staged NV12 sources are persistent, but direct NV12 creates/imports/binds a
new Vulkan image each frame. Hardware that genuinely supports direct NV12 therefore retains the
per-frame churn this library is intended to remove.

Fix: route direct NV12 through the same identity/topology/active-claim cache with its strategy in
the key and idle-only eviction.

Relevant code: `rust/video-interop/src/vulkan/mod.rs:1330-1394`.

### P3 — Replace stringly typed Vulkan errors and split the 5K-line module

Statistics classify errors with `starts_with`, so message wording silently changes counters. The
single Vulkan module combines public schemas, capability inventory, cache policy, allocations,
pipelines, and tests.

Fix:

- Introduce `VulkanImportError` variants for validation, allocation mismatch, cache collision,
  active reuse, saturation, Vulkan result, and device loss.
- Match variants for statistics; keep `Display` strings for logs.
- Split into `capability`, `layout`, `cache`, `allocation`, `staging`, and `sync` modules.

### P3 — Tighten schema/documentation edges

- The top-level README's 2560x1440 NV12 example still publishes `5_529_600`; the proven transfer
  path needs the truthful `5_529_856` allocation while visible planes end at `5_529_600`.
- Structural descriptor validation permits unreferenced objects, causing unnecessary FD duplication
  and expanding the retained-handle surface. Reject unreferenced objects unless a documented
  multi-layer use case requires them.
- `stats/2` scans every lease and holder even though `max_active` defaults to infinity. Maintain
  active holder count and oldest-lease ordering incrementally if large owners remain supported.

## Proposed implementation order

1. CI Vulkan/shader gates and focused regression tests.
2. Ready-semaphore cleanup, import-id binding, sync state checks, and queue-submit authority.
3. Exact FD size proof and 32-bit compute-span validation.
4. Typed errors and capability resolver; preserve the now-working pinned transfer recipe.
5. Dispatcher send-error policy and bounded issue/release-failure protocol.
6. Public lifecycle normalization and release-callback isolation.
7. Direct NV12 caching, module split, and documentation cleanup.

Every Vulkan batch must retain the current pinned-RPi5 `LinearBufferToOptimalYuvPlanes` path and be
requalified with exact pixels, validation/MMU logs, synchronization faults, pool/cache statistics,
and shutdown tests.
