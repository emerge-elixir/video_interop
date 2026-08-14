use std::fs::File;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Mutex;
use std::time::Duration;

use rustler::types::reference::Reference;
use rustler::{LocalPid, Resource, ResourceArc, Term};
use video_interop::{
    AcquireSync, ClaimedVideoFrame, Colorimetry, Descriptor, DispatcherHealth, DispatcherProbe,
    Format, Frame, Modifier, OwnedStorage, ReleaseDispatcher, Storage,
    is_abandonment_guard_resource, new_abandonment_guard as make_guard,
};

struct TestFd {
    _fd: OwnedFd,
}

#[rustler::resource_impl]
impl Resource for TestFd {}

struct DispatcherOwner {
    dispatcher: Mutex<Option<ResourceArc<ReleaseDispatcher>>>,
}

#[rustler::resource_impl]
impl Resource for DispatcherOwner {}

struct TestDispatcherProbe(DispatcherProbe);

#[rustler::resource_impl]
impl Resource for TestDispatcherProbe {}

struct NativeClaim {
    frame: Mutex<Option<ClaimedVideoFrame>>,
}

#[rustler::resource_impl]
impl Resource for NativeClaim {}

type DescriptorSummary = (u32, usize, usize, u32, Option<u64>, usize);
type FrameSummary = (u32, u32, u32, u32, bool, bool, bool);

#[rustler::nif]
fn inspect_descriptor(descriptor: Descriptor) -> Result<DescriptorSummary, String> {
    descriptor.validate().map_err(|error| error.to_string())?;
    let first_object = descriptor
        .objects
        .first()
        .ok_or_else(|| "validated descriptor has no object".to_string())?;
    let first_layer = descriptor
        .layers
        .first()
        .ok_or_else(|| "validated descriptor has no layer".to_string())?;

    Ok((
        descriptor.version,
        descriptor.objects.len(),
        descriptor.layers.len(),
        first_layer.fourcc,
        first_object.modifier.explicit(),
        first_layer.planes.len(),
    ))
}

#[rustler::nif]
fn inspect_frame(frame: Frame<'_>) -> Result<FrameSummary, String> {
    let descriptor = match &frame.storage {
        Storage::DmaBuf(descriptor) => descriptor,
        _ => return Err("unsupported storage".to_string()),
    };
    descriptor.validate().map_err(|error| error.to_string())?;

    let modifier_is_linear = descriptor
        .objects
        .first()
        .is_some_and(|object| matches!(object.modifier, Modifier::Explicit(0)));

    Ok((
        frame.coded_width,
        frame.coded_height,
        frame.visible_rect.width,
        frame.visible_rect.height,
        matches!(frame.acquire_sync, AcquireSync::SyncFile(_)),
        frame.lease.token.is_ref() && modifier_is_linear,
        frame.lease.abandonment_guard.is_map(),
    ))
}

#[rustler::nif]
fn round_trip_format(format: Format) -> Result<Format, String> {
    format.validate().map_err(|error| error.to_string())?;
    Ok(format)
}

#[rustler::nif]
fn round_trip_colorimetry(colorimetry: Colorimetry) -> Colorimetry {
    colorimetry
}

#[rustler::nif]
fn start_dispatcher() -> Result<
    (
        ResourceArc<DispatcherOwner>,
        ResourceArc<TestDispatcherProbe>,
    ),
    String,
> {
    let dispatcher =
        ReleaseDispatcher::start("vi-schema-prod").map_err(|error| error.to_string())?;
    let probe = dispatcher.probe();

    Ok((
        ResourceArc::new(DispatcherOwner {
            dispatcher: Mutex::new(Some(dispatcher)),
        }),
        ResourceArc::new(TestDispatcherProbe(probe)),
    ))
}

#[rustler::nif(schedule = "DirtyIo")]
fn shutdown_dispatcher(owner: ResourceArc<DispatcherOwner>) -> Result<bool, String> {
    close_dispatcher(&owner, 5_000)
}

#[rustler::nif(schedule = "DirtyIo")]
fn shutdown_dispatcher_timeout(
    owner: ResourceArc<DispatcherOwner>,
    timeout_ms: u64,
) -> Result<bool, String> {
    close_dispatcher(&owner, timeout_ms)
}

fn close_dispatcher(owner: &DispatcherOwner, timeout_ms: u64) -> Result<bool, String> {
    let dispatcher = dispatcher(owner)?;
    dispatcher
        .close_and_join(Duration::from_millis(timeout_ms))
        .map_err(|error| error.to_string())?;
    Ok(true)
}

#[rustler::nif]
fn delay_dispatcher_for_test(
    owner: ResourceArc<DispatcherOwner>,
    delay_ms: u64,
) -> Result<bool, String> {
    dispatcher(&owner)?
        .inject_dispatch_delay_for_test(Duration::from_millis(delay_ms))
        .map_err(|error| error.to_string())?;
    Ok(true)
}

#[rustler::nif]
fn dispatcher_health(probe: ResourceArc<TestDispatcherProbe>) -> &'static str {
    match probe.0.health() {
        DispatcherHealth::Healthy => "healthy",
        DispatcherHealth::Stopping => "stopping",
        DispatcherHealth::Stopped => "stopped",
        DispatcherHealth::Failed => "failed",
    }
}

#[rustler::nif]
fn new_abandonment_guard_resource<'a>(
    dispatcher_owner: ResourceArc<DispatcherOwner>,
    owner: LocalPid,
    token: Term<'a>,
    holder: Reference<'a>,
) -> Result<ResourceArc<video_interop::AbandonmentGuard>, String> {
    let dispatcher = dispatcher(&dispatcher_owner)?;
    make_guard(dispatcher, owner, token, holder).map_err(|error| error.to_string())
}

#[rustler::nif]
fn abandonment_guard_resource(resource: Term<'_>) -> bool {
    is_abandonment_guard_resource(resource)
}

#[rustler::nif]
fn fail_dispatcher_startup() -> Result<bool, String> {
    ReleaseDispatcher::inject_startup_failure_for_test()
        .map(|_dispatcher| true)
        .map_err(|error| error.to_string())
}

#[rustler::nif]
fn fatal_enqueue_after_publication<'a>(
    dispatcher_owner: ResourceArc<DispatcherOwner>,
    owner: LocalPid,
    token: Term<'a>,
    holder: Reference<'a>,
) -> Result<bool, String> {
    let dispatcher = dispatcher(&dispatcher_owner)?;
    let guard =
        make_guard(dispatcher.clone(), owner, token, holder).map_err(|error| error.to_string())?;
    dispatcher.inject_enqueue_failure_for_test();
    drop(guard);
    Ok(true)
}

#[rustler::nif]
fn fatal_worker_panic(dispatcher_owner: ResourceArc<DispatcherOwner>) -> Result<bool, String> {
    dispatcher(&dispatcher_owner)?.inject_worker_panic_for_test();
    Ok(true)
}

#[rustler::nif(schedule = "DirtyIo")]
fn open_test_fd() -> Result<(i32, ResourceArc<TestFd>), String> {
    let file = File::open("/dev/null").map_err(|error| error.to_string())?;
    let fd: OwnedFd = file.into();
    let raw_fd = fd.as_raw_fd();
    Ok((raw_fd, ResourceArc::new(TestFd { _fd: fd })))
}

#[rustler::nif(schedule = "DirtyIo")]
fn prepare_and_drop_frame(
    frame: Frame<'_>,
    dispatcher_owner: ResourceArc<DispatcherOwner>,
) -> Result<bool, String> {
    let dispatcher = dispatcher(&dispatcher_owner)?;
    let prepared = frame
        .prepare_cloexec(&dispatcher)
        .map_err(|error| error.to_string())?;
    let duplicated_fd = match &prepared.frame().storage {
        OwnedStorage::DmaBuf(descriptor) => descriptor
            .objects
            .first()
            .ok_or_else(|| "prepared descriptor has no object".to_string())?
            .fd
            .as_raw_fd(),
        _ => return Err("unsupported storage".to_string()),
    };

    drop(prepared);
    // SAFETY: F_GETFD only observes whether dropping the prepared frame closed
    // its duplicated descriptor.
    Ok(unsafe { libc::fcntl(duplicated_fd, libc::F_GETFD) } == -1)
}

#[rustler::nif(schedule = "DirtyIo")]
fn claim_frame(
    frame: Frame<'_>,
    dispatcher_owner: ResourceArc<DispatcherOwner>,
) -> Result<ResourceArc<NativeClaim>, String> {
    let dispatcher = dispatcher(&dispatcher_owner)?;
    let prepared = frame
        .prepare_cloexec(&dispatcher)
        .map_err(|error| error.to_string())?;

    Ok(ResourceArc::new(NativeClaim {
        frame: Mutex::new(Some(prepared.claim())),
    }))
}

#[rustler::nif(schedule = "DirtyIo")]
fn claim_and_drop_frame(
    frame: Frame<'_>,
    dispatcher_owner: ResourceArc<DispatcherOwner>,
) -> Result<bool, String> {
    let dispatcher = dispatcher(&dispatcher_owner)?;
    let prepared = frame
        .prepare_cloexec(&dispatcher)
        .map_err(|error| error.to_string())?;
    drop(prepared.claim());
    Ok(true)
}

#[rustler::nif(schedule = "DirtyIo")]
fn retire_frame(
    frame: Frame<'_>,
    dispatcher_owner: ResourceArc<DispatcherOwner>,
) -> Result<bool, String> {
    let dispatcher = dispatcher(&dispatcher_owner)?;
    let prepared = frame
        .prepare_cloexec(&dispatcher)
        .map_err(|error| error.to_string())?;
    let (owned_frame, lease) = prepared.claim().into_parts();
    lease.retire();
    drop(owned_frame);
    Ok(true)
}

#[rustler::nif(schedule = "DirtyIo")]
fn retire_claim(claim: ResourceArc<NativeClaim>) -> Result<bool, String> {
    let claimed = claim
        .frame
        .lock()
        .map_err(|_| "native claim lock poisoned".to_string())?
        .take();

    if let Some(claimed) = claimed {
        let (owned_frame, lease) = claimed.into_parts();
        lease.retire();
        drop(owned_frame);
    }

    Ok(true)
}

fn dispatcher(owner: &DispatcherOwner) -> Result<ResourceArc<ReleaseDispatcher>, String> {
    owner
        .dispatcher
        .lock()
        .map_err(|_| "dispatcher owner lock poisoned".to_string())?
        .as_ref()
        .cloned()
        .ok_or_else(|| "dispatcher owner is shut down".to_string())
}

rustler::init!("Elixir.VideoInterop.SchemaNative");
