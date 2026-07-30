use std::io;

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("coded frame size must be positive, got {width}x{height}")]
    ZeroCodedSize { width: u32, height: u32 },
    #[error("visible frame size must be positive, got {width}x{height}")]
    ZeroVisibleSize { width: u32, height: u32 },
    #[error("visible rectangle arithmetic overflowed")]
    VisibleRectOverflow,
    #[error(
        "visible rectangle ({x}, {y}, {width}, {height}) exceeds coded size {coded_width}x{coded_height}"
    )]
    VisibleRectOutOfBounds {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        coded_width: u32,
        coded_height: u32,
    },
    #[error("unsupported descriptor version {0}")]
    UnsupportedDescriptorVersion(u32),
    #[error("descriptor has no objects")]
    EmptyObjects,
    #[error("descriptor has no layers")]
    EmptyLayers,
    #[error("descriptor has {actual} {kind}; maximum is {maximum}")]
    TooManyEntries {
        kind: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("object {index} has negative fd {fd}")]
    NegativeFd { index: usize, fd: i32 },
    #[error("acquire fence has negative fd {0}")]
    NegativeAcquireFence(i32),
    #[error("object {index} has zero size")]
    ZeroObjectSize { index: usize },
    #[error("layer {index} has invalid DRM fourcc 0")]
    InvalidFourcc { index: usize },
    #[error("layer {index} has no planes")]
    EmptyPlanes { index: usize },
    #[error("descriptor has {actual} total planes; maximum is {maximum}")]
    TooManyPlanes { actual: usize, maximum: usize },
    #[error("layer {layer} plane {plane} has zero pitch")]
    ZeroPitch { layer: usize, plane: usize },
    #[error(
        "layer {layer} plane {plane} references object {object_index}, but only {object_count} objects exist"
    )]
    InvalidObjectIndex {
        layer: usize,
        plane: usize,
        object_index: u32,
        object_count: usize,
    },
    #[error(
        "layer {layer} plane {plane} offset {offset} is outside object {object_index} of size {object_size}"
    )]
    PlaneOffsetOutOfBounds {
        layer: usize,
        plane: usize,
        object_index: u32,
        offset: u64,
        object_size: u64,
    },
}

#[derive(Debug, Error)]
pub enum DuplicateError {
    #[error(transparent)]
    InvalidFrame(#[from] ValidationError),
    #[error("acquire fence has negative fd {0}")]
    NegativeAcquireFence(i32),
    #[error("failed to duplicate object {index} fd {fd}: {source}")]
    DuplicateObjectFd {
        index: usize,
        fd: i32,
        #[source]
        source: io::Error,
    },
    #[error("failed to duplicate acquire-fence fd {fd}: {source}")]
    DuplicateAcquireFence {
        fd: i32,
        #[source]
        source: io::Error,
    },
}

#[cfg(feature = "rustler")]
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("failed to start the video-interop lease release worker: {message}")]
pub struct ReleaseWorkerError {
    pub(crate) message: String,
}

#[cfg(feature = "rustler")]
#[derive(Debug, Error)]
pub enum PrepareError {
    #[error(transparent)]
    Duplicate(#[from] DuplicateError),
    #[error(transparent)]
    ReleaseWorker(#[from] ReleaseWorkerError),
}
