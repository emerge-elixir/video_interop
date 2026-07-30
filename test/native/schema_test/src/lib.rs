use std::fs::File;
use std::os::fd::{AsRawFd, OwnedFd};

use rustler::{Resource, ResourceArc};
use video_interop::{AcquireSync, Descriptor, Frame, Modifier, OwnedStorage, Storage};

struct TestFd {
    _fd: OwnedFd,
}

#[rustler::resource_impl]
impl Resource for TestFd {}

type DescriptorSummary = (u32, usize, usize, u32, Option<u64>, usize);
type FrameSummary = (u32, u32, u32, u32, bool, bool);

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
    ))
}

#[rustler::nif(schedule = "DirtyIo")]
fn open_test_fd() -> Result<(i32, ResourceArc<TestFd>), String> {
    let file = File::open("/dev/null").map_err(|error| error.to_string())?;
    let fd: OwnedFd = file.into();
    let raw_fd = fd.as_raw_fd();
    Ok((raw_fd, ResourceArc::new(TestFd { _fd: fd })))
}

#[rustler::nif(schedule = "DirtyIo")]
fn prepare_and_drop_frame(frame: Frame<'_>) -> Result<bool, String> {
    let prepared = frame.prepare_cloexec().map_err(|error| error.to_string())?;
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
fn claim_and_drop_frame(frame: Frame<'_>) -> Result<bool, String> {
    let prepared = frame.prepare_cloexec().map_err(|error| error.to_string())?;
    drop(prepared.claim());
    Ok(true)
}

#[rustler::nif(schedule = "DirtyIo")]
fn retire_frame(frame: Frame<'_>) -> Result<bool, String> {
    let prepared = frame.prepare_cloexec().map_err(|error| error.to_string())?;
    let (owned_frame, lease) = prepared.claim().into_parts();
    lease.retire();
    drop(owned_frame);
    Ok(true)
}

rustler::init!("Elixir.VideoInterop.SchemaNative");
