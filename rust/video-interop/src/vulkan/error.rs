use thiserror::Error;

/// Structured errors used by Vulkan import policy and statistics.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum VulkanImportError {
    #[error("Vulkan {pool} pool is saturated at {limit} slots")]
    PoolSaturated { pool: &'static str, limit: usize },
    #[error(
        "DMA-BUF declared allocation size {declared} does not match fd-backed allocation size {observed}"
    )]
    AllocationSizeMismatch { declared: u64, observed: u64 },
    #[error("{0}")]
    AllocationSizeProbe(String),
    #[error("{0}")]
    AllocationSize(String),
    #[error("{0}")]
    Other(String),
}

impl VulkanImportError {
    pub fn is_allocation_size(&self) -> bool {
        matches!(
            self,
            Self::AllocationSizeMismatch { .. }
                | Self::AllocationSizeProbe(_)
                | Self::AllocationSize(_)
        )
    }
}
