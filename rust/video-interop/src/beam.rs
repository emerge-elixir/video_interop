use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rustler::env::{OwnedEnv, SavedTerm};
use rustler::types::reference::Reference;
use rustler::{Encoder, LocalPid, NifStruct, Resource, ResourceArc, Term};

use crate::{
    AcquireSync, DispatcherError, FrameDescriptor, OwnedFrame, PrepareError, Rect, Storage,
};

mod atoms {
    rustler::atoms! {
        video_interop_release,
        video_interop_abandoned
    }
}

const HEALTHY: u8 = 0;
const STOPPING: u8 = 1;
const STOPPED: u8 = 2;
const FAILED: u8 = 3;

#[derive(Clone, NifStruct)]
#[module = "VideoInterop.Lease"]
pub struct Lease<'a> {
    pub owner: LocalPid,
    pub token: Term<'a>,
    pub holder: Reference<'a>,
    pub abandonment_guard: Term<'a>,
}

#[derive(Clone, NifStruct)]
#[module = "VideoInterop.Frame"]
pub struct Frame<'a> {
    pub coded_width: u32,
    pub coded_height: u32,
    pub visible_rect: Rect,
    pub storage: Storage,
    pub acquire_sync: AcquireSync,
    pub lease: Lease<'a>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatcherHealth {
    Healthy,
    Stopping,
    Stopped,
    Failed,
}

enum MessageKind {
    Release,
    Abandoned,
}

struct DispatchCommand {
    kind: MessageKind,
    owner: Option<LocalPid>,
    environment: OwnedEnv,
    token: Option<SavedTerm>,
    holder: Option<SavedTerm>,
}

enum WorkerCommand {
    Dispatch(DispatchCommand),
    Stop,
    #[cfg(feature = "test-support")]
    DelayForTest(Duration),
    #[cfg(feature = "test-support")]
    FailForTest,
}

struct DispatcherState {
    worker: Option<JoinHandle<()>>,
    stop_enqueued: bool,
    joining: bool,
}

/// Lifecycle-owned dispatcher for deterministic and fallback lease messages.
///
/// A producer or consumer owns the root Rustler resource. Prepared and claimed
/// leases hold counted clients that pin the resource until deterministic
/// release has been queued. Guards also pin the resource, but become inert
/// after an exact lifecycle close so stale already-retired BEAM terms cannot
/// keep shutdown waiting forever.
///
/// The lifecycle owner must call [`ReleaseDispatcher::close_and_join`] from a
/// dirty-I/O NIF after its exact holder/claim drain. Resource destructors never
/// wait or join. Dropping the last reference before an explicit join is fatal,
/// because otherwise a worker could execute unloaded NIF code.
pub struct ReleaseDispatcher {
    sender: Sender<WorkerCommand>,
    state: Mutex<DispatcherState>,
    health: Arc<AtomicU8>,
    undelivered_commands: Arc<AtomicUsize>,
    active_clients: AtomicUsize,
}

#[derive(Clone)]
pub struct DispatcherProbe {
    health: Arc<AtomicU8>,
    undelivered_commands: Arc<AtomicUsize>,
}

impl DispatcherProbe {
    pub fn health(&self) -> DispatcherHealth {
        decode_health(self.health.load(Ordering::Acquire))
    }

    pub fn undelivered_commands(&self) -> usize {
        self.undelivered_commands.load(Ordering::Acquire)
    }
}

#[rustler::resource_impl]
impl Resource for ReleaseDispatcher {}

impl ReleaseDispatcher {
    /// Starts a dispatcher and returns its lifecycle-owner resource.
    pub fn start(name: impl Into<String>) -> Result<ResourceArc<Self>, DispatcherError> {
        let (sender, receiver) = channel();
        let health = Arc::new(AtomicU8::new(HEALTHY));
        let worker_health = Arc::clone(&health);
        let undelivered_commands = Arc::new(AtomicUsize::new(0));
        let worker_undelivered = Arc::clone(&undelivered_commands);

        let worker = thread::Builder::new()
            .name(name.into())
            .spawn(move || dispatch_worker(receiver, worker_health, worker_undelivered))
            .map_err(|error| DispatcherError::new(error.to_string()))?;

        Ok(ResourceArc::new(Self {
            sender,
            state: Mutex::new(DispatcherState {
                worker: Some(worker),
                stop_enqueued: false,
                joining: false,
            }),
            health,
            undelivered_commands,
            active_clients: AtomicUsize::new(0),
        }))
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn inject_startup_failure_for_test() -> Result<ResourceArc<Self>, DispatcherError> {
        Err(DispatcherError::new("injected dispatcher startup failure"))
    }

    pub fn health(&self) -> DispatcherHealth {
        decode_health(self.health.load(Ordering::Acquire))
    }

    pub fn probe(&self) -> DispatcherProbe {
        DispatcherProbe {
            health: Arc::clone(&self.health),
            undelivered_commands: Arc::clone(&self.undelivered_commands),
        }
    }

    /// Stops admission, waits for all prepared/claimed clients, drains the FIFO,
    /// and joins the worker. This function blocks and must only be called by a
    /// lifecycle-owned dirty-I/O NIF (or a native non-BEAM thread).
    ///
    /// A timeout leaves the dispatcher in `Stopping`; the owner resource and
    /// worker remain live and a later call may retry the join.
    pub fn close_and_join(&self, timeout: Duration) -> Result<(), DispatcherError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| DispatcherError::new("dispatcher close timeout is too large"))?;
        self.begin_close()?;
        self.wait_for_clients(deadline)?;
        self.enqueue_stop_once(deadline)?;
        self.wait_for_worker_exit_and_join(deadline)
    }

    fn begin_close(&self) -> Result<(), DispatcherError> {
        loop {
            match decode_health(self.health.load(Ordering::SeqCst)) {
                DispatcherHealth::Healthy => {
                    if self
                        .health
                        .compare_exchange(HEALTHY, STOPPING, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                    {
                        return Ok(());
                    }
                }
                DispatcherHealth::Stopping | DispatcherHealth::Stopped => return Ok(()),
                DispatcherHealth::Failed => {
                    return Err(DispatcherError::new("dispatcher is Failed"));
                }
            }
        }
    }

    fn wait_for_clients(&self, deadline: Instant) -> Result<(), DispatcherError> {
        while self.active_clients.load(Ordering::SeqCst) != 0 {
            sleep_until_retry(deadline).map_err(|_| {
                DispatcherError::new("timed out waiting for dispatcher clients to retire")
            })?;
        }
        Ok(())
    }

    fn enqueue_stop_once(&self, deadline: Instant) -> Result<(), DispatcherError> {
        let mut state = self.lock_state_until(deadline)?;
        if self.health() == DispatcherHealth::Stopped || state.stop_enqueued {
            return Ok(());
        }
        if state.worker.is_none() {
            return Err(DispatcherError::new("dispatcher worker is unavailable"));
        }
        if self.sender.send(WorkerCommand::Stop).is_err() {
            fatal_dispatcher_corruption("dispatcher worker stopped before lifecycle join");
        }
        state.stop_enqueued = true;
        Ok(())
    }

    fn wait_for_worker_exit_and_join(&self, deadline: Instant) -> Result<(), DispatcherError> {
        loop {
            if self.health() == DispatcherHealth::Stopped {
                return Ok(());
            }
            if self.health() == DispatcherHealth::Failed {
                return Err(DispatcherError::new("dispatcher is Failed"));
            }

            let worker = {
                let mut state = self.lock_state_until(deadline)?;
                match state.worker.as_ref() {
                    Some(worker) if !state.joining && worker.is_finished() => {
                        state.joining = true;
                        state.worker.take()
                    }
                    Some(_) | None if state.joining => None,
                    Some(_) => None,
                    None => {
                        return Err(DispatcherError::new("dispatcher worker is unavailable"));
                    }
                }
            };

            if let Some(worker) = worker {
                if worker.thread().id() == thread::current().id() {
                    fatal_dispatcher_corruption("dispatcher attempted to join its own worker");
                }
                // `is_finished()` proved the thread has exited, so this join only
                // collects its result and cannot wait for FIFO work.
                if worker.join().is_err() {
                    fatal_dispatcher_corruption("dispatcher worker panicked during shutdown");
                }
                // Publish completion only after `join` collected the already-finished
                // worker. Concurrent closers can now return without touching the
                // lifecycle mutex, even if this caller is descheduled afterward.
                self.health.store(STOPPED, Ordering::SeqCst);
                return Ok(());
            }

            sleep_until_retry(deadline).map_err(|_| {
                DispatcherError::new("timed out waiting for dispatcher worker to drain")
            })?;
        }
    }

    fn lock_state_until(
        &self,
        deadline: Instant,
    ) -> Result<MutexGuard<'_, DispatcherState>, DispatcherError> {
        loop {
            match self.state.try_lock() {
                Ok(state) => return Ok(state),
                Err(TryLockError::Poisoned(_)) => {
                    return Err(DispatcherError::new("dispatcher state lock poisoned"));
                }
                Err(TryLockError::WouldBlock) => sleep_until_retry(deadline).map_err(|_| {
                    DispatcherError::new("timed out waiting for dispatcher close ownership")
                })?,
            }
        }
    }

    fn acquire_client(dispatcher: &ResourceArc<Self>) -> Result<DispatcherClient, DispatcherError> {
        if decode_health(dispatcher.health.load(Ordering::SeqCst)) != DispatcherHealth::Healthy {
            return Err(DispatcherError::new(format!(
                "dispatcher is {:?}",
                dispatcher.health()
            )));
        }

        dispatcher.active_clients.fetch_add(1, Ordering::SeqCst);
        if decode_health(dispatcher.health.load(Ordering::SeqCst)) == DispatcherHealth::Healthy {
            Ok(DispatcherClient {
                dispatcher: dispatcher.clone(),
            })
        } else {
            dispatcher.release_client();
            Err(DispatcherError::new(format!(
                "dispatcher is {:?}",
                dispatcher.health()
            )))
        }
    }

    fn release_client(&self) {
        let previous = self.active_clients.fetch_sub(1, Ordering::SeqCst);
        if previous == 0 {
            fatal_dispatcher_corruption("dispatcher client count underflow");
        }
    }

    fn with_guard_admission<T>(
        dispatcher: &ResourceArc<Self>,
        operation: impl FnOnce() -> T,
    ) -> Result<T, DispatcherError> {
        let permit = Self::acquire_client(dispatcher)?;
        let result = operation();
        drop(permit);
        Ok(result)
    }

    fn dispatch_abandoned(dispatcher: &ResourceArc<Self>, command: DispatchCommand) {
        match Self::acquire_client(dispatcher) {
            Ok(client) => client.dispatch(command),
            Err(_)
                if matches!(
                    dispatcher.health(),
                    DispatcherHealth::Stopping | DispatcherHealth::Stopped
                ) =>
            {
                // Exact lifecycle drainage makes a late guard for an already
                // retired holder a safe no-op after admission closes.
            }
            Err(_) => fatal_dispatcher_corruption(
                "abandonment guard could not acquire its published dispatcher",
            ),
        }
    }

    fn sender_for_published(&self) -> Sender<WorkerCommand> {
        self.sender.clone()
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn inject_dispatch_delay_for_test(&self, delay: Duration) -> Result<(), DispatcherError> {
        if self.health() != DispatcherHealth::Healthy {
            return Err(DispatcherError::new(format!(
                "dispatcher is {:?}",
                self.health()
            )));
        }
        self.sender
            .send(WorkerCommand::DelayForTest(delay))
            .map_err(|_| DispatcherError::new("dispatcher worker stopped unexpectedly"))
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn inject_worker_panic_for_test(&self) {
        let command = DispatchCommand::panic_for_test();
        let sender = self.sender_for_published();
        if sender.send(WorkerCommand::Dispatch(command)).is_err() {
            fatal_dispatcher_corruption("dispatcher worker stopped unexpectedly");
        }
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn inject_enqueue_failure_for_test(&self) {
        self.health.store(FAILED, Ordering::Release);
        let _ = self.sender.send(WorkerCommand::FailForTest);
    }
}

impl Drop for ReleaseDispatcher {
    fn drop(&mut self) {
        if self.health.load(Ordering::Acquire) != STOPPED {
            fatal_dispatcher_corruption("dispatcher owner dropped without explicit close_and_join");
        }
    }
}

struct DispatcherClient {
    dispatcher: ResourceArc<ReleaseDispatcher>,
}

impl DispatcherClient {
    fn dispatch(self, command: DispatchCommand) {
        let sender = self.dispatcher.sender_for_published();
        if sender.send(WorkerCommand::Dispatch(command)).is_err() {
            fatal_dispatcher_corruption("dispatcher worker stopped unexpectedly");
        }
    }
}

impl Drop for DispatcherClient {
    fn drop(&mut self) {
        self.dispatcher.release_client();
    }
}

/// Unique fallback resource attached to one canonical holder.
pub struct AbandonmentGuard {
    dispatcher: ResourceArc<ReleaseDispatcher>,
    command: Mutex<Option<DispatchCommand>>,
}

#[rustler::resource_impl]
impl Resource for AbandonmentGuard {}

impl Drop for AbandonmentGuard {
    fn drop(&mut self) {
        let command = match self.command.get_mut() {
            Ok(command) => command.take(),
            Err(_) => fatal_dispatcher_corruption("abandonment guard lock poisoned"),
        };

        if let Some(command) = command {
            ReleaseDispatcher::dispatch_abandoned(&self.dispatcher, command);
        }
    }
}

/// Creates the resource used by a thin producer-NIF guard constructor.
///
/// Guard creation is pre-publication and reports ordinary admission errors.
/// Once returned to BEAM, enqueue loss while accepting is fatal lifecycle
/// corruption. After exact lifecycle close, a stale guard is inert.
/// Returns whether a term is this producer NIF's registered guard resource.
/// Producer authority callbacks should expose this constant-time check.
pub fn is_abandonment_guard_resource(term: Term<'_>) -> bool {
    term.decode::<ResourceArc<AbandonmentGuard>>().is_ok()
}

pub fn new_abandonment_guard<'a>(
    dispatcher: ResourceArc<ReleaseDispatcher>,
    owner: LocalPid,
    token: Term<'a>,
    holder: Reference<'a>,
) -> Result<ResourceArc<AbandonmentGuard>, DispatcherError> {
    ReleaseDispatcher::with_guard_admission(&dispatcher, || {
        ResourceArc::new(AbandonmentGuard {
            dispatcher: dispatcher.clone(),
            command: Mutex::new(Some(DispatchCommand::new(
                MessageKind::Abandoned,
                owner,
                token,
                Term::from(holder),
            ))),
        })
    })
}

pub struct PreparedLease {
    command: DispatchCommand,
    guard: GuardKeepalive,
    client: DispatcherClient,
}

pub struct ClaimedLease {
    command: Option<DispatchCommand>,
    guard: Option<GuardKeepalive>,
    client: Option<DispatcherClient>,
}

pub struct PreparedVideoFrame {
    frame: OwnedFrame,
    lease: PreparedLease,
}

pub struct ClaimedVideoFrame {
    pub frame: OwnedFrame,
    pub lease: ClaimedLease,
}

struct GuardKeepalive {
    _environment: OwnedEnv,
    _guard: SavedTerm,
}

impl Lease<'_> {
    fn prepare(&self, client: DispatcherClient) -> PreparedLease {
        let command = DispatchCommand::new(
            MessageKind::Release,
            self.owner,
            self.token,
            Term::from(self.holder),
        );

        let guard_environment = OwnedEnv::new();
        let guard = guard_environment.save(self.abandonment_guard);

        PreparedLease {
            command,
            guard: GuardKeepalive {
                _environment: guard_environment,
                _guard: guard,
            },
            client,
        }
    }
}

impl Frame<'_> {
    /// Validates and duplicates all borrowed fds while leaving lease release
    /// ownership with the Elixir caller.
    ///
    /// The entire abandonment authority envelope is saved opaquely. Call
    /// [`PreparedVideoFrame::claim`] only after the native subsystem has
    /// accepted responsibility for eventual retirement.
    pub fn prepare_cloexec(
        &self,
        dispatcher: &ResourceArc<ReleaseDispatcher>,
    ) -> Result<PreparedVideoFrame, PrepareError> {
        let client = ReleaseDispatcher::acquire_client(dispatcher)?;

        let descriptor = FrameDescriptor {
            coded_width: self.coded_width,
            coded_height: self.coded_height,
            visible_rect: self.visible_rect.clone(),
            storage: self.storage.clone(),
            acquire_sync: self.acquire_sync.clone(),
        };

        let frame = descriptor.duplicate_cloexec()?;
        let lease = self.lease.prepare(client);

        Ok(PreparedVideoFrame { frame, lease })
    }
}

impl PreparedVideoFrame {
    pub fn frame(&self) -> &OwnedFrame {
        &self.frame
    }

    /// Transfers deterministic release responsibility and the opaque guard
    /// envelope to native code.
    pub fn claim(self) -> ClaimedVideoFrame {
        ClaimedVideoFrame {
            frame: self.frame,
            lease: ClaimedLease {
                command: Some(self.lease.command),
                guard: Some(self.lease.guard),
                client: Some(self.lease.client),
            },
        }
    }
}

impl ClaimedVideoFrame {
    pub fn into_parts(self) -> (OwnedFrame, ClaimedLease) {
        (self.frame, self.lease)
    }
}

impl ClaimedLease {
    /// Queues deterministic producer release. The guard envelope remains live
    /// until the worker accepts the release command.
    pub fn retire(mut self) {
        self.dispatch_release();
    }

    fn dispatch_release(&mut self) {
        if let Some(command) = self.command.take() {
            let client = self.client.take().unwrap_or_else(|| {
                fatal_dispatcher_corruption("claimed lease lost its dispatcher client")
            });
            client.dispatch(command);
            drop(self.guard.take());
        }
    }
}

impl Drop for ClaimedLease {
    fn drop(&mut self) {
        self.dispatch_release();
    }
}

impl DispatchCommand {
    fn new(kind: MessageKind, owner: LocalPid, token: Term<'_>, holder: Term<'_>) -> Self {
        let environment = OwnedEnv::new();
        let token = environment.save(token);
        let holder = environment.save(holder);

        Self {
            kind,
            owner: Some(owner),
            environment,
            token: Some(token),
            holder: Some(holder),
        }
    }

    #[cfg(feature = "test-support")]
    fn panic_for_test() -> Self {
        Self {
            kind: MessageKind::Release,
            owner: None,
            environment: OwnedEnv::new(),
            token: None,
            holder: None,
        }
    }

    fn send(mut self) -> Result<(), rustler::env::SendError> {
        let owner = self
            .owner
            .take()
            .unwrap_or_else(|| panic!("injected dispatcher worker panic"));
        let tag = match self.kind {
            MessageKind::Release => atoms::video_interop_release(),
            MessageKind::Abandoned => atoms::video_interop_abandoned(),
        };
        let token = self.token.take().expect("dispatch token missing");
        let holder = self.holder.take().expect("dispatch holder missing");

        self.environment.send_and_clear(&owner, move |env| {
            (tag, token.load(env), holder.load(env)).encode(env)
        })
    }
}

fn dispatch_worker(
    receiver: Receiver<WorkerCommand>,
    health: Arc<AtomicU8>,
    undelivered_commands: Arc<AtomicUsize>,
) {
    let result = catch_unwind(AssertUnwindSafe(|| {
        loop {
            match receiver.recv() {
                Ok(WorkerCommand::Dispatch(command)) => {
                    if command.send().is_err() {
                        // The local recipient exited. Its producer/owner-crash destructor is now
                        // authoritative; the dispatcher worker and FIFO remain healthy.
                        undelivered_commands.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Ok(WorkerCommand::Stop) => break,
                #[cfg(feature = "test-support")]
                Ok(WorkerCommand::DelayForTest(delay)) => thread::sleep(delay),
                #[cfg(feature = "test-support")]
                Ok(WorkerCommand::FailForTest) => break,
                Err(_) => {
                    if health.load(Ordering::Acquire) != STOPPING {
                        fatal_dispatcher_corruption(
                            "dispatcher worker stopped outside lifecycle shutdown",
                        );
                    }
                    break;
                }
            }
        }
    }));

    if result.is_err() {
        health.store(FAILED, Ordering::Release);
        fatal_dispatcher_corruption("dispatcher worker panicked");
    }
}

fn sleep_until_retry(deadline: Instant) -> Result<(), ()> {
    let remaining = deadline.checked_duration_since(Instant::now()).ok_or(())?;
    if remaining.is_zero() {
        return Err(());
    }
    thread::sleep(remaining.min(Duration::from_millis(1)));
    Ok(())
}

fn decode_health(value: u8) -> DispatcherHealth {
    match value {
        HEALTHY => DispatcherHealth::Healthy,
        STOPPING => DispatcherHealth::Stopping,
        STOPPED => DispatcherHealth::Stopped,
        _ => DispatcherHealth::Failed,
    }
}

fn fatal_dispatcher_corruption(reason: &str) -> ! {
    eprintln!("video_interop fatal dispatcher corruption: {reason}");
    process::abort()
}
