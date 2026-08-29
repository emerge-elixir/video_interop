#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DmaBufIdentity {
    pub device: u64,
    pub inode: u64,
    pub allocation_size: u64,
}

use crate::dmabuf::probe_dmabuf;

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
    let observed = probe_dmabuf(source_fd)
        .map_err(|error| VulkanImportError::AllocationSizeProbe(format!("Vulkan {error}")))?;
    if declared_size != observed.allocation_size {
        return Err(VulkanImportError::AllocationSizeMismatch {
            declared: declared_size,
            observed: observed.allocation_size,
        });
    }
    Ok(DmaBufIdentity {
        device: observed.device,
        inode: observed.inode,
        allocation_size: observed.allocation_size,
    })
}
