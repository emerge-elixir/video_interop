use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use rustler::env::{OwnedEnv, SavedTerm};
use rustler::types::reference::Reference;
use rustler::{Encoder, LocalPid, NifStruct, Term};

use crate::{
    AcquireSync, FrameDescriptor, OwnedFrame, PrepareError, Rect, ReleaseWorkerError, Storage,
};

mod atoms {
    rustler::atoms! {
        video_interop_release
    }
}

#[derive(Clone, NifStruct)]
#[module = "VideoInterop.Lease"]
pub struct Lease<'a> {
    pub owner: LocalPid,
    pub token: Term<'a>,
    pub holder: Reference<'a>,
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

struct ReleaseCommand {
    owner: LocalPid,
    environment: OwnedEnv,
    token: SavedTerm,
    holder: SavedTerm,
}

pub struct PreparedLease {
    command: ReleaseCommand,
}

pub struct ClaimedLease {
    command: Option<ReleaseCommand>,
}

pub struct PreparedVideoFrame {
    frame: OwnedFrame,
    lease: PreparedLease,
}

pub struct ClaimedVideoFrame {
    pub frame: OwnedFrame,
    pub lease: ClaimedLease,
}

static RELEASE_DISPATCHER: OnceLock<Result<Sender<ReleaseCommand>, ReleaseWorkerError>> =
    OnceLock::new();

impl Lease<'_> {
    fn prepare(&self) -> PreparedLease {
        let environment = OwnedEnv::new();
        let token = environment.save(self.token);
        let holder = environment.save(Term::from(self.holder));

        PreparedLease {
            command: ReleaseCommand {
                owner: self.owner,
                environment,
                token,
                holder,
            },
        }
    }
}

impl Frame<'_> {
    /// Validates and duplicates all borrowed fds while leaving lease release
    /// ownership with the Elixir caller.
    ///
    /// Call [`PreparedVideoFrame::claim`] only after the native subsystem has
    /// accepted responsibility for eventual retirement. Dropping an unclaimed
    /// prepared frame closes its duplicated fds but sends no lease release.
    pub fn prepare_cloexec(&self) -> Result<PreparedVideoFrame, PrepareError> {
        ensure_release_dispatcher()?;

        let descriptor = FrameDescriptor {
            coded_width: self.coded_width,
            coded_height: self.coded_height,
            visible_rect: self.visible_rect.clone(),
            storage: self.storage.clone(),
            acquire_sync: self.acquire_sync.clone(),
        };

        let frame = descriptor.duplicate_cloexec()?;
        let lease = self.lease.prepare();

        Ok(PreparedVideoFrame { frame, lease })
    }
}

impl PreparedVideoFrame {
    /// Borrows the duplicated native frame for validation and admission checks.
    /// The frame cannot be extracted before lease ownership is claimed.
    pub fn frame(&self) -> &OwnedFrame {
        &self.frame
    }

    /// Transfers deterministic lease-release responsibility to native code.
    ///
    /// A claimed lease queues `{:video_interop_release, token, holder}` from a
    /// dedicated native thread when explicitly retired or dropped.
    pub fn claim(self) -> ClaimedVideoFrame {
        ClaimedVideoFrame {
            frame: self.frame,
            lease: ClaimedLease {
                command: Some(self.lease.command),
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
    /// Queues deterministic producer release. The actual BEAM message is sent
    /// by the crate's dedicated native release worker.
    pub fn retire(mut self) {
        if let Some(command) = self.command.take() {
            enqueue_release(command);
        }
    }
}

impl Drop for ClaimedLease {
    fn drop(&mut self) {
        if let Some(command) = self.command.take() {
            enqueue_release(command);
        }
    }
}

fn ensure_release_dispatcher() -> Result<&'static Sender<ReleaseCommand>, ReleaseWorkerError> {
    match RELEASE_DISPATCHER.get_or_init(start_release_dispatcher) {
        Ok(sender) => Ok(sender),
        Err(error) => Err(error.clone()),
    }
}

fn start_release_dispatcher() -> Result<Sender<ReleaseCommand>, ReleaseWorkerError> {
    let (sender, receiver) = channel();

    thread::Builder::new()
        .name("video-interop-release".to_string())
        .spawn(move || release_worker(receiver))
        .map_err(|error| ReleaseWorkerError {
            message: error.to_string(),
        })?;

    Ok(sender)
}

fn release_worker(receiver: Receiver<ReleaseCommand>) {
    while let Ok(command) = receiver.recv() {
        let result = catch_unwind(AssertUnwindSafe(|| command.send()));

        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("video_interop lease release failed: {error:?}"),
            Err(_panic) => eprintln!("video_interop lease release panicked"),
        }
    }
}

impl ReleaseCommand {
    fn send(mut self) -> Result<(), rustler::env::SendError> {
        let token = self.token;
        let holder = self.holder;

        self.environment.send_and_clear(&self.owner, move |env| {
            (
                atoms::video_interop_release(),
                token.load(env),
                holder.load(env),
            )
                .encode(env)
        })
    }
}

fn enqueue_release(command: ReleaseCommand) {
    match ensure_release_dispatcher() {
        Ok(sender) => {
            if sender.send(command).is_err() {
                eprintln!("video_interop lease release worker stopped unexpectedly");
            }
        }
        Err(error) => eprintln!("video_interop lease release worker unavailable: {error}"),
    }
}
