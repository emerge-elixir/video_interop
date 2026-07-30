#![doc = include_str!("../README.md")]

mod dmabuf;
mod error;
mod fd;
mod frame;
mod geometry;
mod modifier;
mod sync_file;

#[cfg(feature = "rustler")]
mod beam;

pub use dmabuf::{
    AV_DRM_MAX_ENTRIES, Descriptor, Layer, Object, OwnedDescriptor, OwnedObject, Plane,
};
pub use error::{DuplicateError, ValidationError};
pub use frame::{FrameDescriptor, OwnedFrame, OwnedStorage, Storage};
pub use geometry::Rect;
pub use modifier::Modifier;
pub use sync_file::{AcquireSync, OwnedAcquireSync, SyncFile};

pub(crate) use fd::duplicate_fd_cloexec;

#[cfg(feature = "rustler")]
pub use beam::{ClaimedLease, ClaimedVideoFrame, Frame, Lease, PreparedLease, PreparedVideoFrame};
#[cfg(feature = "rustler")]
pub use error::{PrepareError, ReleaseWorkerError};
