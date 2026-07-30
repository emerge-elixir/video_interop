# Library-Owned Video Lifecycle and Emerge Integration

Status: Phase 1 generic ownership/consumer foundation implemented; independent
review and downstream phases remain.

This plan supersedes the application-owned `PrimeBridge` and `PrimeRenderer`
approach in `/workspace/emerge_demo`. It refines Phase 2 and the consumer side of
`membrane-video-interop-migration.md`.

## Goal

Applications configure video producers, consumers, and Membrane links. They do
not implement:

- DMA-BUF descriptor conversion;
- lease issue, retain, release, or retirement messages;
- native keepalive routing;
- frame rejection cleanup;
- renderer-drain waiting or forced shutdown loops;
- target-incarnation checks;
- synchronization ownership transitions.

The resulting boundaries are:

```text
video_interop
  generic frame/format/storage/sync schemas
  generic lease ownership, fan-out, consumption receipts, and draining
  Rust prepare -> claim -> exact retirement

Emerge
  emits and consumes VideoInterop.Frame
  owns VideoTarget validation and native submission
  owns direct headless-output -> VideoTarget connections
  guarantees that renderer stop drains

membrane_video_interop
  maps VideoInterop.Frame to/from Membrane.Buffer
  provides a reusable ownership-safe Membrane consumer sink
  contains no Emerge-specific or generic native implementation

applications
  create targets and connect them
  declare Membrane source |> sink graphs
```

## Architecture decisions

### Use direct Emerge connection for `emerge_demo`

Do not insert Membrane between two Emerge renderers only to remove a custom
bridge. Emerge owns both endpoints and can provide the smaller, safer API:

```elixir
{:ok, target} = EmergeSkia.video_target(window_renderer, ...)
:ok = Emerge.connect_video_output(EmergeDemo.PrimeSource, target)
```

`emerge_demo` validates Emerge's headless PRIME export and generic frame import.
It will not contain `PrimeBridge`, `PrimeRenderer`, a Membrane pipeline, or frame
lifecycle code.

### Use Membrane only at real Membrane boundaries

`membrane_video_interop` is exercised by real graphs such as:

```text
MembraneLibcamera.Source
  -> Membrane.VideoInterop.Sink
  -> EmergeSkia.VideoTarget
```

and:

```text
RTP/decode source
  -> VideoInterop buffers
  -> Membrane.VideoInterop.Sink
  -> EmergeSkia.VideoTarget
```

The visual acceptance application is `/workspace/emerge_video_demo`, followed
by the Colibri camera application. Do not add a generic mailbox-push Membrane
source in this slice: crash-safe external admission, bounded queueing, and
ownership acknowledgement require a separate design. Existing camera and
decoder plugins are already proper Membrane sources.

### Keep Emerge independent from Membrane

Emerge depends on `video_interop` and the `video-interop` crate directly. It
must not depend on `membrane_video_interop` or `membrane_core`.

`membrane_video_interop` depends on `video_interop` and `membrane_core`, but not
on Emerge. It consumes any implementation of the generic consumer protocol.
Emerge implements that protocol for `%EmergeSkia.VideoTarget{}` in the Emerge
repository, so dependency direction remains one-way.

### No compatibility translation

Never translate an old `%Membrane.DMABuf.VideoFrame{}` into a
`%VideoInterop.Frame{}` while preserving the old lease. Old and new lease atoms
are incompatible. Migrate each producer/consumer closure atomically and use a
cold restart or complete drain.

## Public API design

### 1. Generic frame helpers

Add frame-level fan-out to `VideoInterop` so callers never rebuild a frame with
a manually retained lease:

```elixir
@spec retain(VideoInterop.Frame.t(), timeout()) ::
        {:ok, VideoInterop.Frame.t()} | {:error, term()}

def retain(frame, timeout \\ 5_000)

@spec release(VideoInterop.Frame.t() | VideoInterop.Lease.t()) :: :ok
```

`retain/2` returns the same immutable frame metadata/storage/sync with a unique
child lease holder. It does not duplicate FDs and does not wait on acquire
synchronization. The original and child frames must each be retired exactly
once.

### 2. Ownership-aware consumer sessions

A consumer session is required, not only a one-frame callback. The session
provides an identity for pending/current native claims and lets a library close
the last displayed frame at disconnect, EOS, reconnection, owner death, or
producer shutdown.

Add two protocols:

```elixir
defprotocol VideoInterop.Consumer do
  @spec open(t(), VideoInterop.Format.t(), keyword()) ::
          {:ok, VideoInterop.ConsumerSession.t()} | {:error, term()}
  def open(consumer, format, opts)
end

defprotocol VideoInterop.ConsumerSession do
  @spec transfer(t(), VideoInterop.Frame.t()) ::
          {:ok, :transferred | :released}
          | {:error, {:caller_owned | :transferred, term()}}
  def transfer(session, frame)

  @spec close(t()) :: :ok
  def close(session)
end
```

`Consumer.open/3` receives `owner: pid()` and creates a unique stream/session
identity. Implementations monitor that owner or provide an equivalent native
resource destructor. `ConsumerSession.close/1` is idempotent and has an
infallible postcondition: admission for that identity is closed and all of its
pending/current claims are already retired or scheduled for consumer-safe
retirement before `:ok` returns. Missing, stale, or renderer-closed registry
entries count as success because they can no longer own admitted claims.

Emerge implements close as an atomic registry operation with no fallible work
after the local session changes to closed. Its native session resource also
performs the same idempotent close on owner-process failure/drop. It must never
return an error that asks the application to retry; failure to establish the
postcondition is an internal fatal invariant violation reported by Emerge while
the resource destructor continues cleanup.

Add public helpers:

```elixir
VideoInterop.open_consumer(consumer, format, owner: self())
VideoInterop.consume(session, frame)
VideoInterop.close_consumer(session)
```

Ownership meanings for `ConsumerSession.transfer/2` are:

| Result | Holder owner after return | Caller action |
| --- | --- | --- |
| `{:ok, :transferred}` | consumer/native subsystem | never release |
| `{:ok, :released}` | already retired by consumer | never release |
| `{:error, {:caller_owned, reason}}` | caller | release or retry |
| `{:error, {:transferred, reason}}` | consumer/native subsystem | never release |

`VideoInterop.consume/2` releases only a `:caller_owned` error and normalizes the
receipt. Once called, its caller never releases the supplied holder after any
normal return.

Before dispatch, `consume/2` checks that a session protocol implementation
exists. A missing implementation is a known pre-transfer error and releases the
frame. An implementation exception or invalid receipt raises
`VideoInterop.ConsumerContractError` with ownership `:unknown`; neither the
helper nor a Membrane adapter guesses by releasing. Trusted library
implementations must contain exceptions, perform every fallible operation before
claim, and return an ownership receipt on every path. Forced failure tests cover
both preclaim and immediately-after-claim boundaries.

### 3. Safe producer issue results

Change `VideoInterop.LeaseOwner.issue/3` to:

```elixir
@spec issue(pid(), backend_token, keyword()) ::
        {:ok, VideoInterop.Lease.t()}
        | {:error, {:caller_owned | :transferred, term()}}
```

Rules:

1. Monitor and preflight the local owner before sending.
2. If no request was sent, return `{:error, {:caller_owned, reason}}`.
3. The send operation is the ownership boundary.
4. Every timeout, rejection, release failure, or owner death after send returns
   `{:error, {:transferred, reason}}`.
5. Remove aliases and monitor messages on every return path.
6. Backend tokens require an independent owner-crash/message-drop destructor
   fallback because BEAM cannot prove whether a concurrently dying PID consumed
   a sent message.

Producer code releases the private backend token only after a `:caller_owned`
error. It never releases it after success or a `:transferred` error.

### 4. Blocking generic drain

Keep `LeaseOwner.close/2` as nonblocking “stop admission and begin drain.” Add:

```elixir
@spec VideoInterop.LeaseOwner.drain(pid(), timeout()) ::
        :ok
        | {:error,
           :timeout
           | {:owner_down, term()}
           | {:release_failed, reference(), term()}}
```

The exact owner state machine is:

```text
:open
  -- close/drain/producer exit --> :draining
:draining
  -- final successful callback --> reply waiters, notify, stop :normal
:draining
  -- final callback failure -----> retain failed token for retry
```

Rules:

1. The owner mailbox's receive order is authoritative. Once the drain transition
   is processed, reject both new issues and new retains. Requests processed
   before it complete normally; pending handshakes are cancelled or confirmed
   before drain can complete.
2. `drain/2` uses an alias-based request and registers both its waiter and waiter
   monitor inside the owner before evaluating completion.
3. Success replies to registered waiters before the owner stops normally.
4. Timeout sends an ordered waiter cancellation and removes only that waiter.
   It never stops the owner; the owner may independently complete concurrently.
5. A release failure replies current drain waiters with the exact public token
   and reason and keeps the owner alive for `retry/3`.
6. A successful retry follows the normal completion rule. If it was the last
   failed/active token, `retry/3` success is itself proof of completed drainage
   and the owner may stop; callers must not require a later `drain/2` against a
   dead PID.
7. Waiter death removes the waiter without changing drain state.
8. A caller that was never registered and finds a dead PID receives
   `{:owner_down, reason}`; a bare PID does not provide durable post-exit
   queryability.

Do not implement `drain/2` as `close/2` followed by an unregistered monitor.

A process must not block in `drain/2` while it owns a holder or while its mailbox
is required to process retirement. Actor-style producers such as Emerge's
headless session use nonblocking `close/2` and finish from owner notifications.

`LeaseOwner` also owns optional release retry so producer libraries do not each
implement timers:

```elixir
release_retry: :manual
release_retry: {:exponential, initial_ms: 10, max_ms: 1_000,
                max_attempts: :infinity}
```

There is at most one timer/invocation per failed public token. Timers carry a
generation and stale timers are ignored after manual retry, success, or owner
termination. Retry callbacks use the same private backend token and therefore
must be idempotent. Exhaustion keeps the exact token/reason retryable and reports
failure to drain waiters/observers. Public `retry/3` normalizes timeout and owner
death into error tuples; it never exits its caller. A retry racing normal owner
completion returns success or `{:owner_down, :normal}` without reviving work.

Emerge, video-surface, and libcamera producer owners use infinite bounded
exponential retry plus diagnostics. If a producer cannot make its backend
release idempotent, it must terminate through its backend-token destructor
fallback instead of enabling automatic retry; applications never own this
choice.

### 5. Emerge frame submission

Add the consuming stream API:

```elixir
@spec EmergeSkia.submit_video_frame(
        EmergeSkia.VideoConsumerSession.t(),
        VideoInterop.Frame.t()
      ) :: :ok | {:error, term()}
```

This operation consumes the supplied holder on every normal return. Callers do
not construct descriptor maps, pass `owner_pid`, receive `{:keepalive, ...}`, or
release after an error.

Emerge implements `VideoInterop.Consumer` for
`%EmergeSkia.VideoTarget{}`. Opening starts an unlinked, Emerge-owned
`VideoConsumerSession` actor with a unique native stream resource and owner
monitor. The actor serializes transfer versus close; owner death invokes the
same idempotent native close. The native stream resource also closes on drop as
the actor-crash fallback. The session validates target mode, geometry,
supported format/layout, and renderer liveness. Its `transfer/2` uses an
internal ownership-tagged native boundary and its `close/1` stops admission
before retiring that stream's final displayed claim.
`submit_video_frame/2` delegates to `VideoInterop.consume/2` for an already-open
session. Direct Emerge connections and the reusable Membrane sink own session
open/close; applications do not.

Retain `submit_prime/2` only as a deprecated raw-map compatibility API until all
local consumers migrate. Do not use it in new code.

### 6. Direct Emerge output connection

Add:

```elixir
@spec Emerge.connect_video_output(
        GenServer.server(),
        EmergeSkia.VideoTarget.t(),
        keyword()
      ) :: {:ok, reference()} | {:error, term()}

def connect_video_output(source, target, opts \\ [])

@spec Emerge.disconnect_video_output(GenServer.server()) :: :ok | {:error, term()}
```

Options initially contain only `notify: pid() | nil`. A successful call returns
`{:ok, connection_ref}`. When configured, the headless session sends:

```elixir
{:emerge_video_output, source_pid, connection_ref, :connected}
{:emerge_video_output, source_pid, connection_ref,
 {:first_frame_accepted, sequence}}
{:emerge_video_output, source_pid, connection_ref, :disconnected}
{:emerge_video_output, source_pid, connection_ref, {:error, reason}}
```

Messages for one connection are sent by the source session in that order;
reconnection creates a new reference so applications can ignore late status
from an old connection. “Accepted” means ownership transferred to the consumer,
not imported, sampled, or displayed. Notifications are UI/diagnostic facts and
never request lifecycle work.

The call routes through `Emerge.Runtime.Viewport` and the renderer behaviour.
Only a headless PRIME renderer accepts it. Other renderers return
`{:error, :video_output_unsupported}`. Target dimensions/mode are checked before
installation.

The headless session destination state is:

```text
:disconnected
{:external, pid, monitor_ref}       # deprecated low-level output contract
{:consumer, VideoConsumerSession, notify_pid}
```

Connecting opens a consumer session owned by the headless session. Disconnect,
reconnect, source stop, producer death, and submission-terminal error first
close that consumer session, which stops admission and schedules its
pending/current claims for GPU-safe retirement. Only then does producer lease
drain begin. This explicit close guarantees that the consumer's final displayed
frame cannot hold the producer open forever.

While disconnected, release the native backend token before issuing a public
lease. Reconnection closes the old consumer session before installing the new
one; already claimed old frames retire through the old session's close path.
Stale target incarnations are rejected before claim or safely retired by the
session.

External PID delivery remains only as a deprecated advanced producer contract
for non-Emerge integrations. Its recipient must release every accepted frame,
and synchronous source stop is conditional on those holders retiring. Recipient
process death is not retirement. `emerge_demo` and application-facing examples
must not use this mode.

### 7. Synchronous Emerge shutdown

`EmergeSkia.stop/1` for a headless PRIME session must not acknowledge only the
start of drainage.

`HeadlessPrimeSession.handle_call(:stop, from, state)` stores all stop callers
and performs this ordered transition:

1. set session mode to `:draining` so later native frames release raw backend
   tokens without issuing leases;
2. close the active consumer session, stopping admission and scheduling the
   final displayed claim for GPU-safe retirement;
3. call nonblocking `LeaseOwner.close/2`;
4. continue handling late native frames, consumer retirement releases, and
   release retries;
5. after the lease owner reports drained, call `Native.stop/1`;
6. reply to all stop callers and terminate normally.

Normal `HeadlessPrimeSession.stop/1` uses `GenServer.call(..., :infinity)`.
The default `Emerge.Runtime.Viewport.Renderer.Skia` uses this behavior, so no
application renderer wrapper is needed. Emerge viewport child specs already use
`shutdown: :infinity`.

Emerge configures the lease owner's infinite bounded-exponential release retry.
On `:video_interop_lease_release_failed`, the session records
token/reason/age/attempt and emits diagnostics; it does not create competing
retry timers. Native backend-token release is idempotent so retry cannot
re-release a reused slot. Correctness-preserving stop waits through retries;
applications never implement retry loops.

Provide an explicit bounded diagnostic waiter if needed:

```elixir
EmergeSkia.stop(renderer, timeout: milliseconds)
```

Implement this with a monitored/alias stop waiter and an internal timer, not a
finite `GenServer.call` that exits the caller. Timeout returns
`{:error, {:drain_timeout, stats}}`, removes that waiter, and leaves the session
safely draining/retrying. It must not kill the session or invalidate live
buffers. Other unbounded stop waiters remain registered.

If `Native.stop/1` fails, reply with an Emerge-owned shutdown error containing
native diagnostics and keep/terminate the session according to whether native
admission and resources are proven closed. Do not report success or ask the
application to release frames manually.
## Native Emerge claim boundary

Replace Emerge's hand-built descriptor-map keepalive with the Rust
`video-interop` types.

The NIF path is:

```text
Elixir owns %VideoInterop.Frame{}
  -> decode and validate
  -> duplicate every DMA-BUF/sync-file FD CLOEXEC
  -> PreparedVideoFrame (Elixir still owns lease)
  -> validate target, incarnation, dimensions, format, sync support, capacity
  -> atomically insert into native pending/import state and claim()
  -> ClaimedVideoFrame (native owns lease)
  -> drop at rejection/replacement/context loss/GPU-display retirement
  -> native release worker sends :video_interop_release
```

All fallible admission checks occur before `claim()`. After claim, the function
must return a transferred receipt and leave a claimed object stored in a path
whose drop retires it. If a future post-claim error is unavoidable, dropping
the claim first and returning `{:error, {:transferred, reason}}` is required.
The NIF result is exactly the generic ownership receipt; it must not return an
untagged error or raise across the claim boundary.

Initial accepted frame shape is deliberately narrow and checked before claim:

- DMA-BUF storage with exactly one layer;
- ABGR8888/`AB24` with one plane or NV12 with two planes;
- object indices, offsets, pitches, object sizes, and per-plane spans accepted by
  `video-interop` validation;
- coded dimensions equal target dimensions;
- full-frame `visible_rect` at `{0, 0}` until crop sampling is implemented;
- progressive frames only;
- supported alpha mode for the selected fourcc, with unsupported alpha rejected;
- implicit or supported linear/per-buffer modifiers only under current importer
  capabilities;
- `acquire_sync: :implicit` only until explicit EGL waits ship.

Every unsupported shape returns a caller-owned error and causes no import, wake,
generation change, or redraw.

### Native target and consumer-session identity

Fix target identity before exposing direct connections:

- generate a renderer epoch when each native renderer/registry starts;
- generate a target incarnation for every registration;
- store exact `{renderer_epoch, target_id, incarnation}` in both the registry
  entry and `VideoTargetResource`;
- require exact identity for lookup, submit, stream close, and resource-drop
  removal;
- make stale resource drop incapable of removing a same-ID replacement;
- add atomic registry admission state `:open | :closed`;
- `Native.stop/1` closes admission before backend teardown, retires every
  consumer session/pending/current claim, and rejects later submissions as
  caller-owned;
- retaining a `VideoTarget` resource after renderer stop must not keep admission
  open.

Opening an Emerge consumer session creates a native stream identity beneath one
exact target incarnation. Pending/imported/current claims carry that identity.
Closing the stream atomically rejects later frames for it and schedules all its
claims, including the final displayed frame, for existing GPU-safe cleanup.
One target accepts one active stream in the first implementation; another open
returns `{:error, :target_busy}` until the old stream is closed.

Required properties:

- preparing then rejecting closes duplicated FDs and leaves Elixir ownership;
- inactive targets reject before import, wake, generation change, or redraw;
- pending-frame replacement retires the replaced claim exactly once;
- imported/current frame replacement waits for the existing GPU/display
  retirement point;
- registry removal, renderer shutdown, context loss, and terminal poison drop
  every claim exactly once;
- release delivery remains on the crate's dedicated native worker, never a BEAM
  scheduler thread;
- explicit acquire fences are rejected as caller-owned until the EGL consumer
  wait path is implemented.

## Reusable Membrane sink

Add `%Membrane.VideoInterop.Sink{}` with options:

```elixir
%Membrane.VideoInterop.Sink{
  consumer: video_target,
  on_error: :stop,       # or :drop
  notify_to: self()      # optional diagnostics only
}
```

It has one manual-flow input pad accepting `%VideoInterop.Format{}` and demands
one buffer at a time.

Behavior:

1. Validate the stream format and open a consumer session owned by the sink
   process before demand.
2. On format renegotiation, stop demand, close the old consumer session, then
   validate/open the replacement before resuming.
3. Require the canonical empty-payload `metadata.video_interop` contract.
4. Validate each frame against the negotiated format.
5. Call `VideoInterop.consume(session, frame)`.
6. Demand again only after the ownership result is resolved.
7. On transport or validation rejection, release any canonical embedded
   `%VideoInterop.Frame{}` exactly once before dropping/stopping.
8. On `on_error: :stop`, close the consumer session, then enter terminal drain:
   continue demanding and release every canonical frame without submission
   until producer EOS. Terminate with the saved error only after EOS.
9. On EOS, close the consumer session so its final displayed claim retires.
10. On `on_error: :drop`, notify only after ownership resolution and demand the
    next buffer.
11. Never hold a frame between callbacks and never handle native retirement
    messages itself.

With `notify_to: pid`, the sink sends:

```elixir
{:membrane_video_interop, sink_pid, stream_ref, {:stream_format, format}}
{:membrane_video_interop, sink_pid, stream_ref, :start_of_stream}
{:membrane_video_interop, sink_pid, stream_ref, {:first_frame_accepted, pts}}
{:membrane_video_interop, sink_pid, stream_ref, {:dropped, reason}}
{:membrane_video_interop, sink_pid, stream_ref, {:consumer_error, reason}}
{:membrane_video_interop, sink_pid, stream_ref, :end_of_stream}
```

A new stream format/session creates a new `stream_ref`. The sink sends messages
in callback order and only reports frame acceptance/drop after ownership is
resolved. “Accepted” does not mean displayed. Notifications are diagnostics,
not ownership signals. No demand is issued after the stop decision.

The sink must handle a malformed buffer that still embeds a canonical frame.
`fetch_frame/1` correctly returns `:error`, but the sink must inspect only for
cleanup and release that frame before rejecting it. It must never translate or
release legacy `:dmabuf` terms.

Do not add Emerge dependencies, a lease owner, descriptor structs, native code,
or validation implementations to `membrane_video_interop`.

Membrane core may prefetch canonical buffers into a private input queue. A
terminal frame or format error therefore does not terminate immediately: the
sink re-demands and releases queued holders until producer EOS. Safe orderly
shutdown is producer EOS -> sink terminal drain/EOS close -> pipeline
termination.

Arbitrary `Membrane.Pipeline.terminate/2` before EOS or `Process.exit(pid,
:kill)` can discard a queued frame term whose lease has no BEAM destructor. The
Emerge consumer-session owner monitor can close already-transferred/current
native claims, but it cannot recover such a queued frame. Those operations are
outside this unacknowledged transport contract; use the EOS sequence or a
whole-VM cold restart. A future crash-safe admission custodian requires a
separate acknowledged Membrane transport design.

## Implementation phases

### Phase 0: preserve and freeze

Before changing any consumer repository:

1. Record branch, HEAD, status, lockfiles, and configured remotes for:
   - `/workspace/video_interop`;
   - `/workspace/membrane_video_interop`;
   - `/workspace/emerge-headless`;
   - `/workspace/emerge_demo`;
   - `/workspace/emerge_video_demo`;
   - `/workspace/colibri/membrane_video_surfaces`;
   - `/workspace/colibri/membrane_libcamera`;
   - `/workspace/colibri/camera`.
2. For each dirty worktree with a valid `HEAD` create:
   - a recovery ref for tracked HEAD;
   - `git diff --binary` and staged binary diff;
   - a tar archive of untracked files;
   - SHA-256 manifests for patches and archive contents;
   - an `--include-untracked` stash.
3. For an unborn repository (`video_interop` or `membrane_video_interop` before
   its first commit), create a complete tar archive plus sorted path/size/SHA-256
   manifest, verify a disposable restore byte-for-byte, then make an explicit
   recovery snapshot commit before further mutation. Do not require an
   impossible ref/stash against a missing `HEAD`.
4. Restore each backup into a disposable directory and compare hashes.
5. Record all recovery material in one lockstep manifest outside the live
   worktrees.
6. Preserve `/workspace/colibri/membrane_dmabuf` at `845a697` unchanged.

No schema or lease migration begins until these backups are verified.

### Phase 1: harden `video_interop`

Files:

```text
/workspace/video_interop/lib/video_interop.ex
/workspace/video_interop/lib/video_interop/lease_owner.ex
/workspace/video_interop/lib/video_interop/consumer.ex          # new
/workspace/video_interop/lib/video_interop/consumer_session.ex  # new
/workspace/video_interop/lib/video_interop/consumer_contract_error.ex # new
/workspace/video_interop/test/frame_test.exs
/workspace/video_interop/test/lease_owner_test.exs
/workspace/video_interop/test/consumer_test.exs                 # new
/workspace/video_interop/README.md
/workspace/video_interop/CHANGELOG.md
```

Implement, in order:

1. owner-monitored ownership-tagged `LeaseOwner.issue/3`;
2. atomic waiter-based `LeaseOwner.drain/2` and normalized `retry/3` errors;
3. single-flight optional release retry policy and frame-level
   `VideoInterop.retain/2`;
4. consumer/session protocols, consuming `VideoInterop.consume/2`, and
   idempotent `close_consumer/1`.

Tests must cover:

- already-dead owner before send;
- owner death after send and after registration;
- reply-versus-DOWN ordering with no stale mailbox messages;
- timeout/cancel, capacity, draining, and callback-failure ownership;
- drain with root and child holders;
- drain timeout followed by a second wait when the owner remains alive;
- multiple drain waiters and dead/timed-out waiters;
- close/drain racing a retain, with no new holder admitted after transition;
- timeout racing normal owner completion;
- failed final release followed by successful retry/completion;
- single-flight exponential retry, stale-timer cancellation, exhaustion,
  idempotent callback expectation, and owner-down normalization;
- retain producing a distinct holder over identical frame data;
- all four consumer receipts;
- missing protocol implementation;
- invalid consumer receipt treated as a contract failure without guessed
  release.

Commit and pass all Mix, Cargo, no-default-feature, Clippy, docs, Hex, and Cargo
package gates before downstream migration.

### Phase 2: add the reusable Membrane sink

Files:

```text
/workspace/membrane_video_interop/lib/membrane/video_interop/sink.ex  # new
/workspace/membrane_video_interop/lib/membrane/video_interop.ex
/workspace/membrane_video_interop/test/sink_test.exs                  # new
/workspace/membrane_video_interop/test/buffer_contract_test.exs
/workspace/membrane_video_interop/README.md
/workspace/membrane_video_interop/CHANGELOG.md
```

Use fake consumer implementations plus a real `LeaseOwner` to prove:

- exact direct `%VideoInterop.Format{}` negotiation and rejected format;
- format renegotiation closes the old session before opening the new one;
- serial demand and no buffering;
- successful transfer and synchronous release receipts are not double released;
- caller-owned failure, validation failure, drop, malformed payload with an
  embedded frame, and termination release once;
- transferred failure is not released by the sink;
- legacy/dual metadata is rejected, never translated;
- first-frame and error notifications happen after ownership resolution and
  contain no ownership responsibility;
- EOS closes the consumer session and retires its last claim;
- `on_error: :stop` re-demands/releases prefetched holders until EOS, then
  terminates without a queued holder;
- invalid-format rejection follows the same prefetched-holder drain;
- external pipeline termination is invoked only after producer EOS.

The Hex archive must still contain no Rust, Rustler, NIF, Emerge module,
`LeaseOwner`, or duplicate generic schema.

### Phase 3: make Emerge a canonical consumer

Elixir files:

```text
/workspace/emerge-headless/mix.exs
/workspace/emerge-headless/lib/emerge_skia.ex
/workspace/emerge-headless/lib/emerge_skia/native.ex
/workspace/emerge-headless/lib/emerge_skia/video_target.ex
/workspace/emerge-headless/lib/emerge_skia/video_target_consumer.ex  # new
/workspace/emerge-headless/lib/emerge_skia/video_consumer_session.ex # new
```

Rust files:

```text
/workspace/emerge-headless/native/emerge_skia/Cargo.toml
/workspace/emerge-headless/native/emerge_skia/src/lib.rs
/workspace/emerge-headless/native/emerge_skia/src/video.rs
```

Changes:

- replace `membrane_dmabuf`/`membrane-dmabuf` dependencies with
  `video_interop`/`video-interop`;
- decode `%VideoInterop.Frame{storage:, acquire_sync:, lease:}` directly;
- use `PreparedVideoFrame -> ClaimedVideoFrame` for submission;
- add renderer epoch, target incarnation, registry open/closed state, and exact
  identity checks for submit/removal;
- add native consumer-stream identity and stream-close retirement;
- implement `VideoInterop.Consumer` for `VideoTarget` and
  `VideoInterop.ConsumerSession` for Emerge's session handle;
- expose consuming `EmergeSkia.submit_video_frame/2` for an opened session;
- retain and deprecate raw map `submit_prime/2` temporarily;
- remove `owner_pid` and `keepalive` from the new path.

Focused tests:

- ABGR8888 and NV12 descriptor preparation;
- dimension, visible-rect, interlace, alpha, layer, plane, object-size, modifier,
  and target-incarnation errors;
- same-ID target replacement cannot be submitted to or removed by a stale
  resource;
- submit racing renderer stop, retained target after stop, and registry closed;
- implicit sync accepted and explicit sync rejected without claim;
- inactive target rejection remains import/wake/redraw-free;
- queue admission, replacement, consumer-stream close, registry removal,
  renderer shutdown, and context loss each release once;
- closing a stream retires its final displayed frame at the GPU-safe point;
- prepared drop sends no release;
- claimed drop and explicit retirement send one release;
- no FD growth over repeated admission/rejection/replacement.

### Phase 4: migrate Emerge headless output and stop

Files:

```text
/workspace/emerge-headless/lib/emerge_skia/headless_prime_session.ex
/workspace/emerge-headless/lib/emerge_skia/options.ex
/workspace/emerge-headless/lib/emerge_skia/transport/native.ex
/workspace/emerge-headless/lib/emerge_skia.ex
/workspace/emerge-headless/test/emerge_skia_test.exs
```

Changes:

- emit `%VideoInterop.Frame{storage:, acquire_sync:, lease:}`;
- preserve the association-list key `"dmabuf"` because it names storage mode;
- use the ownership-tagged issue result correctly;
- support disconnected, deprecated external PID, and direct consumer-session
  destinations;
- open/close direct consumer sessions on connect/reconnect/disconnect;
- close the consumer session before closing the producer lease owner;
- consume direct-target frames through the canonical session path;
- internally retry release failures and make stop callers wait for owner
  drainage and native stop;
- preserve linear export, top-left orientation, slot pooling, cadence,
  diagnostics, and backpressure behavior.

Tests:

- disconnected frames release raw backend tokens without issuing leases;
- external output retains the generic `%VideoInterop.Frame{}` contract;
- direct output consumes each frame once;
- connect/reconnect/disconnect with old frames still in flight;
- stale target, same-ID target replacement, and renderer registry close;
- closing the consumer stream retires the final frame without requiring a
  replacement frame;
- producer-first and consumer-first stop with zero, one, and retained child
  leases;
- concurrent/repeated stop callers;
- late native frame during drain;
- producer death, deprecated external target death, lease-owner abnormal exit,
  and release failure with internal retry/backoff;
- bounded diagnostic timeout leaves the session draining and does not remove
  unbounded waiters.

### Phase 5: expose direct viewport connection

Files:

```text
/workspace/emerge-headless/lib/emerge.ex
/workspace/emerge-headless/lib/emerge/runtime/viewport.ex
/workspace/emerge-headless/lib/emerge/runtime/viewport/renderer.ex
/workspace/emerge-headless/lib/emerge/runtime/viewport/renderer/skia.ex
/workspace/emerge-headless/test/emerge/viewport_test.exs
```

Changes:

- add `connect_video_output/3` and `disconnect_video_output/1`;
- route calls to optional renderer callbacks;
- return explicit unsupported/not-ready/wrong-mode/wrong-size/stale-target
  errors;
- keep non-Skia test renderers source-compatible through optional callbacks;
- add optional status notifications without exposing leases;
- test every documented tuple, per-connection ordering, first-frame acceptance
  meaning, reconnect reference rollover, and stale-notification filtering.

### Phase 6: simplify `emerge_demo`

Delete:

```text
/workspace/emerge_demo/lib/emerge_demo/prime_bridge.ex
/workspace/emerge_demo/lib/emerge_demo/prime_renderer.ex
```

Update:

```text
/workspace/emerge_demo/lib/emerge_demo/application.ex
/workspace/emerge_demo/lib/emerge_demo/prime_source.ex
/workspace/emerge_demo/lib/emerge_demo.ex
/workspace/emerge_demo/test/prime_validation_test.exs
/workspace/emerge_demo/test/emerge_demo_test.exs
/workspace/emerge_demo/README.md
```

The source viewport starts under a registered name using the default renderer
and may start disconnected. After the main viewport creates its `VideoTarget`,
it calls one Emerge connection API. The page may display library status
notifications, but it performs no lifecycle action.

Search acceptance for application source:

```text
PrimeBridge                     # none
PrimeRenderer                   # none
Membrane.DMABuf                 # none
VideoInterop.release            # none
owner_pid                       # none
keepalive                       # none
descriptor_from_frame           # none
{:video_interop_release, ...}   # none
```

The live page must still render the animated headless scene and stop cleanly
with frames in flight.

### Phase 7: migrate the real Membrane closure

Migrate atomically:

```text
/workspace/colibri/membrane_video_surfaces
/workspace/colibri/membrane_libcamera
/workspace/emerge_video_demo
/workspace/colibri/camera
```

Producers emit `%VideoInterop.Format{}` and canonical
`metadata.video_interop`. Native code uses the single `video-interop` crate and
claims only after admission. Each producer library makes backend-token release
idempotent and configures the generic single-flight retry policy; if that is
impossible, it owns an explicit fatal destructor-fallback policy. Applications
never retry releases. Replace application-owned Emerge sinks with
`%Membrane.VideoInterop.Sink{consumer: video_target}` where their behavior is
only validation/submission/release. Move synchronous analysis/probe cleanup into
reusable plugin/library consumers so application callbacks receive results, not
lease responsibility. Keep application components only for camera controls,
analysis policy, metrics, and UI state—not frame ownership.

`/workspace/emerge_video_demo` is the first visual adapter acceptance target:
its libcamera and RTP pipelines become ordinary source/decoder-to-reusable-sink
graphs. Then migrate the Colibri camera application and its Nerves closure.

Inventory every producer and graph fan-out. Producers/plugins must issue or
retain one distinct holder per branch before emitting any branch. Ban ordinary
`Membrane.Tee`/raw buffer duplication for these frames. If a migrated graph
needs arbitrary branching, add a lease-aware `Membrane.VideoInterop.Splitter`
as a prerequisite; it synchronously creates `N-1` child holders and rolls back
all created holders on partial admission failure. Applications do not retain
holders manually.

Canonical migration branches contain no legacy `:prime`/`:drm_prime` protocol
modes. Preserve rollback through separate refs, packages, manifests, and a cold
boot—not by shipping both lease protocols in one deployment.

### Phase 8: explicit synchronization

After canonical implicit-sync migration is stable, implement
`active-headless-prime-explicit-sync.md`:

- producer exports an EGL native-fence sync file;
- `%VideoInterop.SyncFile{}` carries it;
- consumer duplicates/imports/waits before sampling;
- claim owns the duplicated acquire fence together with frame storage;
- unsupported paths use `glFinish()` plus `:implicit`;
- acquire completion remains separate from consumer retirement.

Do not combine this with the initial ownership migration.

## Validation gates

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

### `membrane_video_interop`

```bash
VIDEO_INTEROP_PATH=../video_interop mix format --check-formatted
VIDEO_INTEROP_PATH=../video_interop mix test --warnings-as-errors
VIDEO_INTEROP_PATH=../video_interop mix test --cover
VIDEO_INTEROP_PATH=../video_interop mix docs
env -u VIDEO_INTEROP_PATH mix hex.build
```

### Emerge and demos

```bash
cd /workspace/emerge-headless
cargo test --manifest-path native/emerge_skia/Cargo.toml
mix test
./ci-tests.sh all

cd /workspace/emerge_demo
mix test

cd /workspace/emerge_video_demo
mix test
```

Also re-run Emerge's default/no-default/drm feature matrices and prove
raster-only binaries do not dynamically link EGL, GL, or GBM.

### Hardware

On user-accessible DRM hardware validate:

- sustained headless output -> direct Emerge target;
- sustained libcamera -> Membrane sink -> Emerge target;
- target hidden/visible transitions and inactive-drop counters;
- target destruction/recreation and direct reconnection;
- consumer-first and producer-first shutdown with frames in flight;
- producer EOS -> sink drain -> orderly pipeline terminate/restart, plus viewport
  shutdown/restart;
- whole-VM cold restart under load;
- documented evidence that arbitrary in-VM `:kill` is outside the unacknowledged
  Membrane transport guarantee;
- 1,000+ frames with flat FD and active-lease counts;
- exact release count equals accepted producer buffers;
- no import, wake, generation change, or redraw for inactive targets;
- first-frame latency, cadence, and current render-cost diagnostics.

The assistant environment cannot access `/dev/dri`; this gate must run in the
user environment.

## Commit and publication sequence

Keep one intentional writer per worktree and use review-only validation between
commits.

Suggested commits:

1. `video_interop`: harden issue ownership.
2. `video_interop`: add drain, frame retain, and consumer protocol.
3. `membrane_video_interop`: add reusable consumer sink.
4. Emerge: consume canonical frames through prepared/claimed native ownership.
5. Emerge: emit canonical frames and synchronously drain stop.
6. Emerge: add direct viewport output connection.
7. `emerge_demo`: delete bridge/renderer and use direct connection.
8. Membrane producers: atomic canonical contract migration.
9. `emerge_video_demo`: use reusable sink.
10. Camera application/firmware: use reusable sink and cold-cutover contract.

Publication order:

1. crates.io `video-interop`;
2. Hex `video_interop`;
3. Hex `membrane_video_interop`;
4. Emerge;
5. Membrane producer packages;
6. applications and firmware.

No published artifact may contain sibling path dependencies,
`[patch.crates-io]`, old contract structs, or mixed lease atoms.

## Completion criteria

- `emerge_demo` contains neither `PrimeBridge` nor `PrimeRenderer`.
- Application code never constructs native PRIME descriptor maps or handles
  keepalive/release messages.
- `EmergeSkia.submit_video_frame/2` consumes one holder through an opened
  consumer session on every normal result.
- Native claim occurs only after all fallible admission checks.
- `EmergeSkia.stop/1` returns only after lease drain and native stop.
- Direct Emerge output reconnects safely across target incarnations.
- Real Membrane graphs use `%Membrane.VideoInterop.Sink{}` without custom frame
  lifecycle sinks, and no application source contains `VideoInterop.release/1`,
  raw descriptor conversion, lease atoms, or holder fan-out.
- Emerge remains Membrane-independent.
- `membrane_video_interop` remains Elixir-only and generic.
- Old/new contracts never coexist for one holder or rolling deployment.
- Host/package/Nerves/hardware gates pass with flat FD and lease counts.
