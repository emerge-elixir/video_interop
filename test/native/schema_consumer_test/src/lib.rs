use std::sync::Mutex;
use std::time::Duration;

use rustler::{Resource, ResourceArc};
use video_interop::{
    ClaimedVideoFrame, DispatcherHealth, DispatcherProbe, Frame, ReleaseDispatcher,
};

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

#[rustler::nif]
fn start_dispatcher() -> Result<
    (
        ResourceArc<DispatcherOwner>,
        ResourceArc<TestDispatcherProbe>,
    ),
    String,
> {
    let dispatcher =
        ReleaseDispatcher::start("vi-schema-cons").map_err(|error| error.to_string())?;
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
    let dispatcher = dispatcher(&owner)?;
    dispatcher
        .close_and_join(Duration::from_secs(5))
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
fn guard_is_opaque_resource(frame: Frame<'_>) -> bool {
    frame.lease.abandonment_guard.is_map()
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

rustler::init!("Elixir.VideoInterop.SchemaConsumerNative");
