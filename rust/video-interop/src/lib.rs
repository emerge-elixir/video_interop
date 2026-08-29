#![doc = include_str!("../README.md")]

mod dmabuf;
#[cfg(feature = "egl")]
pub mod egl;
mod error;
mod fd;
mod format;
mod frame;
mod geometry;
mod modifier;
mod sync_file;
#[cfg(feature = "vulkan")]
pub mod vulkan;

#[cfg(feature = "rustler")]
mod beam;

pub use dmabuf::{
    AV_DRM_MAX_ENTRIES, Descriptor, Layer, Object, OwnedDescriptor, OwnedObject, Plane,
    dmabuf_allocation_size,
};
pub use error::{DmaBufAllocationSizeError, DuplicateError, ValidationError};
pub use format::{
    AcquireSyncPolicy, AlphaMode, ChromaLocation, ColorRange, Colorimetry, DmaBufFormat, Format,
    FormatValidationError, InterlaceMode, Matrix, ModifierPolicy, Primaries, Rational,
    StreamAcquireSyncPolicy, StreamModifierPolicy, Transfer,
};
pub use frame::{FrameDescriptor, OwnedFrame, OwnedStorage, Storage};
pub use geometry::Rect;
pub use modifier::Modifier;
pub use sync_file::{AcquireSync, OwnedAcquireSync, SyncFile};

pub(crate) use fd::duplicate_fd_cloexec;

#[cfg(feature = "rustler")]
pub use beam::{
    AbandonmentGuard, ClaimedLease, ClaimedVideoFrame, DispatcherHealth, DispatcherProbe, Frame,
    Lease, PreparedLease, PreparedVideoFrame, ReleaseDispatcher, is_abandonment_guard_resource,
    new_abandonment_guard,
};
#[cfg(feature = "rustler")]
pub use error::{DispatcherError, PrepareError, ReleaseWorkerError};
