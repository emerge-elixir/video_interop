use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DmaBufAllocationSizeError {
    #[error("failed to stat DMA-BUF fd: {0}")]
    Stat(#[source] io::Error),
    #[error("failed to query DMA-BUF allocation size: {0}")]
    Seek(#[source] io::Error),
    #[error("failed to restore DMA-BUF file position: {0}")]
    Restore(#[source] io::Error),
    #[error("DMA-BUF allocation size is zero")]
    Zero,
    #[error("DMA-BUF stat size is negative: {0}")]
    NegativeStat(i64),
    #[error("DMA-BUF size probes disagree: fstat={stat}, seek_end={seek_end}")]
    ProbeMismatch { stat: u64, seek_end: u64 },
}

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
    #[error("binary storage has no planes")]
    EmptyBinaryPlanes,
    #[error("binary storage has {0} planes; only one plane is supported")]
    UnsupportedBinaryPlaneCount(usize),
    #[error("binary storage plane has zero stride")]
    ZeroBinaryStride,
    #[error("binary storage plane offset {offset} is outside {data_size} bytes")]
    BinaryOffsetOutOfBounds { offset: u64, data_size: u64 },
    #[error("binary storage last row starts at {offset}, outside {data_size} bytes")]
    BinaryLastRowOutOfBounds { offset: u64, data_size: u64 },
    #[error("binary storage requires implicit synchronization")]
    BinaryStorageRequiresImplicitSync,
    #[error("object {index} is not referenced by any descriptor plane")]
    UnreferencedObject { index: usize },
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
#[error("video-interop release dispatcher unavailable: {message}")]
pub struct DispatcherError {
    pub(crate) message: String,
}

#[cfg(feature = "rustler")]
impl DispatcherError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[cfg(feature = "rustler")]
pub type ReleaseWorkerError = DispatcherError;

#[cfg(feature = "rustler")]
#[derive(Debug, Error)]
pub enum PrepareError {
    #[error("borrowed video frame storage requires a lease")]
    MissingLease,
    #[error("owned binary video frame storage must not carry a lease")]
    UnexpectedLease,
    #[error(transparent)]
    Duplicate(#[from] DuplicateError),
    #[error(transparent)]
    Dispatcher(#[from] DispatcherError),
}
