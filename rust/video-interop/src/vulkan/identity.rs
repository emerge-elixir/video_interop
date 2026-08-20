#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DmaBufIdentity {
    pub device: u64,
    pub inode: u64,
    pub allocation_size: u64,
}

use super::VulkanImportError;

pub(super) fn verified_dmabuf_identity(
    source_fd: i32,
    declared_size: u64,
) -> Result<DmaBufIdentity, VulkanImportError> {
    if source_fd < 0 {
        return Err(VulkanImportError::AllocationSizeProbe(
            "Vulkan DMA-BUF identity requires a valid fd".to_string(),
        ));
    }
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `stat` points to writable storage and `source_fd` remains borrowed by the caller.
    if unsafe { libc::fstat(source_fd, stat.as_mut_ptr()) } != 0 {
        return Err(VulkanImportError::AllocationSizeProbe(format!(
            "failed to stat Vulkan DMA-BUF source fd: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: fstat initialized the complete structure after returning success.
    let stat = unsafe { stat.assume_init() };
    // DMA-BUF exporters expose their allocation through llseek(SEEK_END). Preserve the shared file
    // position when the exporter also supports SEEK_CUR/SEEK_SET.
    let original_position = unsafe { libc::lseek(source_fd, 0, libc::SEEK_CUR) };
    let allocation_end = unsafe { libc::lseek(source_fd, 0, libc::SEEK_END) };
    if original_position >= 0 {
        let _ = unsafe { libc::lseek(source_fd, original_position, libc::SEEK_SET) };
    }
    if allocation_end < 0 {
        return Err(VulkanImportError::AllocationSizeProbe(format!(
            "failed to query Vulkan DMA-BUF allocation size: {}",
            std::io::Error::last_os_error()
        )));
    }
    let allocation_size = u64::try_from(allocation_end).map_err(|_| {
        VulkanImportError::AllocationSizeProbe(
            "Vulkan DMA-BUF allocation size is negative".to_string(),
        )
    })?;
    if stat.st_size > 0 {
        let stat_size = u64::try_from(stat.st_size).map_err(|_| {
            VulkanImportError::AllocationSizeProbe(
                "Vulkan DMA-BUF stat size is negative".to_string(),
            )
        })?;
        if stat_size != allocation_size {
            return Err(VulkanImportError::AllocationSizeProbe(format!(
                "Vulkan DMA-BUF size probes disagree: fstat={stat_size}, seek_end={allocation_size}"
            )));
        }
    }
    if declared_size != allocation_size {
        return Err(VulkanImportError::AllocationSizeMismatch {
            declared: declared_size,
            observed: allocation_size,
        });
    }
    Ok(DmaBufIdentity {
        device: stat.st_dev,
        inode: stat.st_ino,
        allocation_size,
    })
}
