use std::{
    os::fd::IntoRawFd,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use ash::vk;

use super::{
    AcquirePlan, ImportId, ImportedDmaBufImage, StagedAcquirePlan, StagedSampledImages,
    StagedTransferPlan, VulkanDeviceContext, duplicate_import_fd,
};

/// DMA-BUF ownership outside this logical Vulkan device. The core external family is used rather
/// than `FOREIGN_EXT`; producer and consumer must use the same ownership identity.
pub const DMA_BUF_EXTERNAL_QUEUE_FAMILY: u32 = vk::QUEUE_FAMILY_EXTERNAL;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportedImageSyncErrorKind {
    TemporarySemaphoreImport,
    AcquireSubmit,
    ReleaseSubmit,
    ReleaseFenceCreate,
    SourceFencePoll,
    ReleaseFencePoll,
    Other,
}

#[derive(Debug)]
pub struct ImportedImageSyncError {
    kind: ImportedImageSyncErrorKind,
    device_lost: bool,
    message: String,
}

impl ImportedImageSyncError {
    fn new(kind: ImportedImageSyncErrorKind, message: String) -> Self {
        Self {
            kind,
            device_lost: false,
            message,
        }
    }

    fn device_lost(kind: ImportedImageSyncErrorKind, message: String) -> Self {
        Self {
            kind,
            device_lost: true,
            message,
        }
    }

    pub fn kind(&self) -> ImportedImageSyncErrorKind {
        self.kind
    }

    pub fn is_device_lost(&self) -> bool {
        self.device_lost
    }
}

impl std::fmt::Display for ImportedImageSyncError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ImportedImageSyncError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VulkanVideoTiming {
    pub conversion_ns: u64,
    pub composition_ns: u64,
    pub total_gpu_ns: u64,
}

struct TimestampQuery {
    pool: vk::QueryPool,
    period_ns: f64,
    valid_bits: u32,
}

/// One-shot synchronization owner for an imported frame.
///
/// Direct images transition from external `GENERAL` to shader-read and back. Staged imports
/// acquire the external source buffer, run the GPU copy/conversion, return the source buffer to the
/// external family in the same submission, and expose only the internal sampled image to the
/// renderer. The release fence is the only signal that permits lease retirement.
pub struct ImportedImageSync<D: VulkanDeviceContext> {
    device: Arc<D>,
    acquire_pool: vk::CommandPool,
    acquire_command: vk::CommandBuffer,
    release_pool: vk::CommandPool,
    release_command: vk::CommandBuffer,
    acquire_fence: vk::Fence,
    release_fence: vk::Fence,
    imported_acquire: Option<vk::Semaphore>,
    ready_semaphore: Option<vk::Semaphore>,
    import_id: Option<ImportId>,
    staged_import: bool,
    acquire_submitted: bool,
    renderer_accepted: bool,
    renderer_rejected: bool,
    release_submitted: bool,
    timestamp_query: Option<TimestampQuery>,
    timing_active: bool,
    timing_collected: AtomicBool,
    timing: Mutex<Option<VulkanVideoTiming>>,
}

impl<D: VulkanDeviceContext> ImportedImageSync<D> {
    pub fn new(device: Arc<D>) -> Result<Self, ImportedImageSyncError> {
        let acquire_pool =
            create_command_pool(device.as_ref(), "imported-image acquire").map_err(|error| {
                ImportedImageSyncError::new(ImportedImageSyncErrorKind::Other, error)
            })?;
        let release_pool = match create_command_pool(device.as_ref(), "imported-image release") {
            Ok(pool) => pool,
            Err(error) => {
                unsafe { device.device().destroy_command_pool(acquire_pool, None) };
                return Err(ImportedImageSyncError::new(
                    ImportedImageSyncErrorKind::Other,
                    error,
                ));
            }
        };
        let result = (|| {
            let acquire_command =
                allocate_command(device.as_ref(), acquire_pool, "imported-image acquire").map_err(
                    |error| ImportedImageSyncError::new(ImportedImageSyncErrorKind::Other, error),
                )?;
            let release_command =
                allocate_command(device.as_ref(), release_pool, "imported-image release").map_err(
                    |error| ImportedImageSyncError::new(ImportedImageSyncErrorKind::Other, error),
                )?;
            let acquire_fence = unsafe {
                device
                    .device()
                    .create_fence(&vk::FenceCreateInfo::default(), None)
            }
            .map_err(|result| {
                ImportedImageSyncError::new(
                    ImportedImageSyncErrorKind::ReleaseFenceCreate,
                    format!("failed to create imported-image source fence: {result:?}"),
                )
            })?;
            let release_fence = match unsafe {
                device
                    .device()
                    .create_fence(&vk::FenceCreateInfo::default(), None)
            } {
                Ok(fence) => fence,
                Err(result) => {
                    unsafe { device.device().destroy_fence(acquire_fence, None) };
                    return Err(ImportedImageSyncError::new(
                        ImportedImageSyncErrorKind::ReleaseFenceCreate,
                        format!("failed to create imported-image release fence: {result:?}"),
                    ));
                }
            };
            let timestamp_query = match create_timestamp_query(device.as_ref()) {
                Ok(query) => query,
                Err(error) => {
                    unsafe {
                        device.device().destroy_fence(release_fence, None);
                        device.device().destroy_fence(acquire_fence, None);
                    }
                    return Err(error);
                }
            };
            Ok(Self {
                device: Arc::clone(&device),
                acquire_pool,
                acquire_command,
                release_pool,
                release_command,
                acquire_fence,
                release_fence,
                imported_acquire: None,
                ready_semaphore: None,
                import_id: None,
                staged_import: false,
                acquire_submitted: false,
                renderer_accepted: false,
                renderer_rejected: false,
                release_submitted: false,
                timestamp_query,
                timing_active: false,
                timing_collected: AtomicBool::new(false),
                timing: Mutex::new(None),
            })
        })();
        if result.is_err() {
            unsafe {
                device.device().destroy_command_pool(release_pool, None);
                device.device().destroy_command_pool(acquire_pool, None);
            }
        }
        result
    }

    /// Imports a producer sync file as a temporary payload and submits the complete external
    /// acquire operation. The returned semaphore must be accepted and destroyed by the renderer.
    ///
    /// # Safety
    ///
    /// `imported` and every allocation it owns must remain alive until this lane's release fence
    /// completes or the complete device is quarantined and destroyed. Renderer queue use must obey
    /// the context's external-synchronization contract.
    pub unsafe fn submit_acquire(
        &mut self,
        imported: &ImportedDmaBufImage<D>,
        acquire_sync_fd: Option<i32>,
    ) -> Result<vk::Semaphore, ImportedImageSyncError> {
        if self.acquire_submitted || self.release_submitted {
            return Err(ImportedImageSyncError::new(
                ImportedImageSyncErrorKind::Other,
                "imported Vulkan image acquire was already submitted".to_string(),
            ));
        }
        self.timing_active = imported.is_staged();
        let import_id = imported.import_id();
        let imported_acquire = acquire_sync_fd
            .map(|fd| {
                import_temporary_sync_fd(self.device.as_ref(), fd, self.imported_acquire).map_err(
                    |error| {
                        ImportedImageSyncError::new(
                            ImportedImageSyncErrorKind::TemporarySemaphoreImport,
                            error,
                        )
                    },
                )
            })
            .transpose()?;
        if let Some(semaphore) = imported_acquire {
            self.imported_acquire = Some(semaphore);
        }
        let ready = unsafe {
            self.device
                .device()
                .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
        }
        .map_err(|result| {
            ImportedImageSyncError::new(
                ImportedImageSyncErrorKind::Other,
                format!("failed to create imported-image ready semaphore: {result:?}"),
            )
        })?;
        if let Err(result) = unsafe { self.device.device().reset_fences(&[self.acquire_fence]) } {
            unsafe { self.device.device().destroy_semaphore(ready, None) };
            return Err(ImportedImageSyncError::new(
                ImportedImageSyncErrorKind::AcquireSubmit,
                format!("failed to reset staged source fence: {result:?}"),
            ));
        }
        if let Err(error) = self.record_acquire(imported.acquire_plan()) {
            unsafe { self.device.device().destroy_semaphore(ready, None) };
            return Err(ImportedImageSyncError::new(
                ImportedImageSyncErrorKind::Other,
                error,
            ));
        }
        let waits = imported_acquire.into_iter().collect::<Vec<_>>();
        let wait_stages = vec![vk::PipelineStageFlags::ALL_COMMANDS; waits.len()];
        let commands = [self.acquire_command];
        let signals = [ready];
        let submit = vk::SubmitInfo::default()
            .wait_semaphores(&waits)
            .wait_dst_stage_mask(&wait_stages)
            .command_buffers(&commands)
            .signal_semaphores(&signals);
        match unsafe {
            self.device
                .submit_video_queue(&[submit], self.acquire_fence)
        } {
            Ok(()) => {
                imported.mark_acquire_submitted();
                self.import_id = Some(import_id);
                self.staged_import = imported.is_staged();
                self.acquire_submitted = true;
                self.ready_semaphore = Some(ready);
                Ok(ready)
            }
            Err(vk::Result::ERROR_DEVICE_LOST) => {
                // Submission consumption is unknowable. Retain all children in the caller's
                // quarantine owner rather than destroying potentially live semaphore payloads.
                self.import_id = Some(import_id);
                self.staged_import = imported.is_staged();
                self.acquire_submitted = true;
                self.ready_semaphore = Some(ready);
                self.device.mark_device_lost();
                Err(ImportedImageSyncError::device_lost(
                    ImportedImageSyncErrorKind::AcquireSubmit,
                    "Vulkan device lost while acquiring imported image".to_string(),
                ))
            }
            Err(result) => {
                unsafe { self.device.device().destroy_semaphore(ready, None) };
                Err(ImportedImageSyncError::new(
                    ImportedImageSyncErrorKind::AcquireSubmit,
                    format!("failed to submit imported Vulkan image acquire: {result:?}"),
                ))
            }
        }
    }

    fn record_acquire(&mut self, plan: AcquirePlan) -> Result<(), String> {
        unsafe {
            self.device
                .device()
                .reset_command_pool(self.acquire_pool, vk::CommandPoolResetFlags::empty())
        }
        .map_err(|result| format!("failed to reset Vulkan acquire command pool: {result:?}"))?;
        match plan {
            AcquirePlan::DirectImage { image } => {
                record_direct_acquire(self.device.as_ref(), self.acquire_command, image)
            }
            AcquirePlan::StagedCompute(plan) => record_staged_acquire(
                self.device.as_ref(),
                self.acquire_command,
                plan,
                self.timestamp_query.as_ref(),
            ),
            AcquirePlan::StagedTransfer(plan) => record_staged_transfer_acquire(
                self.device.as_ref(),
                self.acquire_command,
                plan,
                self.timestamp_query.as_ref(),
            ),
        }
    }

    /// Polls the staged conversion/source-release submission. A true result proves that the
    /// producer DMA-BUF has been returned to the external queue family and its lease may
    /// retire even while the renderer-native output remains displayed.
    pub fn source_release_complete(&self) -> Result<bool, ImportedImageSyncError> {
        if !self.acquire_submitted {
            return Err(ImportedImageSyncError::new(
                ImportedImageSyncErrorKind::SourceFencePoll,
                "cannot poll staged source release before acquire submission".to_string(),
            ));
        }
        if !self.staged_import {
            return Ok(false);
        }
        match unsafe { self.device.device().get_fence_status(self.acquire_fence) } {
            Ok(signaled) => Ok(signaled),
            Err(vk::Result::ERROR_DEVICE_LOST) => {
                self.device.mark_device_lost();
                Err(ImportedImageSyncError::device_lost(
                    ImportedImageSyncErrorKind::SourceFencePoll,
                    "Vulkan device lost while polling staged source release".to_string(),
                ))
            }
            Err(result) => Err(ImportedImageSyncError::new(
                ImportedImageSyncErrorKind::SourceFencePoll,
                format!("failed to poll staged source-release fence: {result:?}"),
            )),
        }
    }

    pub fn ganesh_wait_accepted(&mut self, semaphore: vk::Semaphore) -> Result<(), String> {
        if self.ready_semaphore != Some(semaphore) {
            return Err("unexpected imported-image ready semaphore".to_string());
        }
        self.ready_semaphore = None;
        self.renderer_accepted = true;
        Ok(())
    }

    /// Records that the renderer rejected the one-shot wait without taking semaphore ownership.
    /// The semaphore remains owned by this lane until the ordered release fence proves its signal
    /// operation complete.
    pub fn ganesh_wait_rejected(&mut self, semaphore: vk::Semaphore) -> Result<(), String> {
        if self.ready_semaphore != Some(semaphore) {
            return Err("unexpected rejected imported-image ready semaphore".to_string());
        }
        self.renderer_rejected = true;
        Ok(())
    }

    /// Queues the lease-retirement fence after renderer work. Direct images also return image
    /// ownership to the external family. Staged inputs already returned the source buffer during
    /// acquire, so their empty fence submission only proves all preceding rendering has completed.
    pub fn submit_release(
        &mut self,
        imported: &ImportedDmaBufImage<D>,
    ) -> Result<(), ImportedImageSyncError> {
        if self.release_submitted {
            return Err(ImportedImageSyncError::new(
                ImportedImageSyncErrorKind::ReleaseSubmit,
                "imported Vulkan image release was already submitted".to_string(),
            ));
        }
        if !self.acquire_submitted || (!self.renderer_accepted && !self.renderer_rejected) {
            return Err(ImportedImageSyncError::new(
                ImportedImageSyncErrorKind::ReleaseSubmit,
                "cannot release an imported Vulkan image before renderer acceptance".to_string(),
            ));
        }
        if self.import_id != Some(imported.import_id()) {
            return Err(ImportedImageSyncError::new(
                ImportedImageSyncErrorKind::ReleaseSubmit,
                format!(
                    "imported Vulkan image release id {} does not match acquired id {}",
                    imported.import_id().get(),
                    self.import_id.map(ImportId::get).unwrap_or(0)
                ),
            ));
        }
        unsafe { self.device.device().reset_fences(&[self.release_fence]) }.map_err(|result| {
            ImportedImageSyncError::new(
                ImportedImageSyncErrorKind::ReleaseSubmit,
                format!("failed to reset imported-image release fence: {result:?}"),
            )
        })?;
        unsafe {
            self.device
                .device()
                .reset_command_pool(self.release_pool, vk::CommandPoolResetFlags::empty())
        }
        .map_err(|result| {
            ImportedImageSyncError::new(
                ImportedImageSyncErrorKind::ReleaseSubmit,
                format!("failed to reset imported-image release pool: {result:?}"),
            )
        })?;
        if imported.is_staged() {
            record_staged_release(
                self.device.as_ref(),
                self.release_command,
                self.timestamp_query.as_ref(),
            )
        } else {
            record_direct_release(self.device.as_ref(), self.release_command, imported.image())
        }
        .map_err(|error| {
            ImportedImageSyncError::new(ImportedImageSyncErrorKind::ReleaseSubmit, error)
        })?;
        let commands = [self.release_command];
        let submit = vk::SubmitInfo::default().command_buffers(&commands);
        match unsafe {
            self.device
                .submit_video_queue(&[submit], self.release_fence)
        } {
            Ok(()) => {
                self.release_submitted = true;
                Ok(())
            }
            Err(vk::Result::ERROR_DEVICE_LOST) => {
                self.device.mark_device_lost();
                Err(ImportedImageSyncError::device_lost(
                    ImportedImageSyncErrorKind::ReleaseSubmit,
                    "Vulkan device lost while releasing imported image".to_string(),
                ))
            }
            Err(result) => Err(ImportedImageSyncError::new(
                ImportedImageSyncErrorKind::ReleaseSubmit,
                format!("failed to submit imported Vulkan image release: {result:?}"),
            )),
        }
    }

    pub fn release_submitted(&self) -> bool {
        self.release_submitted
    }

    pub fn release_complete(&self) -> Result<bool, ImportedImageSyncError> {
        if !self.release_submitted {
            return Ok(false);
        }
        match unsafe { self.device.device().get_fence_status(self.release_fence) } {
            Ok(true) => {
                self.collect_timing()?;
                Ok(true)
            }
            Ok(false) => Ok(false),
            Err(vk::Result::ERROR_DEVICE_LOST) => {
                self.device.mark_device_lost();
                Err(ImportedImageSyncError::device_lost(
                    ImportedImageSyncErrorKind::ReleaseFencePoll,
                    "Vulkan device lost while polling imported-image release".to_string(),
                ))
            }
            Err(result) => Err(ImportedImageSyncError::new(
                ImportedImageSyncErrorKind::ReleaseFencePoll,
                format!("failed to poll imported-image release fence: {result:?}"),
            )),
        }
    }

    pub fn take_timing(&self) -> Option<VulkanVideoTiming> {
        self.timing.lock().ok()?.take()
    }

    pub fn reset_for_reuse(&mut self) -> Result<(), String> {
        if !self.release_submitted {
            return Err("cannot reuse Vulkan image sync before release submission".to_string());
        }
        let complete = unsafe { self.device.device().get_fence_status(self.release_fence) }
            .map_err(|result| format!("failed to prove Vulkan sync lane idle: {result:?}"))?;
        if !complete {
            return Err("cannot reuse Vulkan image sync before exact completion".to_string());
        }
        if self.renderer_rejected
            && let Some(semaphore) = self.ready_semaphore.take()
        {
            unsafe { self.device.device().destroy_semaphore(semaphore, None) };
        }
        if self.ready_semaphore.is_some() {
            return Err(
                "cannot reuse Vulkan image sync with an unaccepted ready semaphore".to_string(),
            );
        }
        self.import_id = None;
        self.staged_import = false;
        self.acquire_submitted = false;
        self.renderer_accepted = false;
        self.renderer_rejected = false;
        self.release_submitted = false;
        self.timing_active = false;
        self.timing_collected.store(false, Ordering::Release);
        *self
            .timing
            .get_mut()
            .map_err(|_| "Vulkan video timing lock poisoned".to_string())? = None;
        Ok(())
    }

    fn collect_timing(&self) -> Result<(), ImportedImageSyncError> {
        if !self.timing_active || self.timing_collected.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let Some(query) = self.timestamp_query.as_ref() else {
            return Ok(());
        };
        let mut values = [0_u64; 3];
        unsafe {
            self.device.device().get_query_pool_results(
                query.pool,
                0,
                &mut values,
                vk::QueryResultFlags::TYPE_64,
            )
        }
        .map_err(|result| {
            ImportedImageSyncError::new(
                ImportedImageSyncErrorKind::ReleaseFencePoll,
                format!("failed to read Vulkan video timestamps: {result:?}"),
            )
        })?;
        let ticks = |start: u64, end: u64| timestamp_delta(start, end, query.valid_bits);
        let to_ns = |ticks: u64| (ticks as f64 * query.period_ns).round() as u64;
        let sample = VulkanVideoTiming {
            conversion_ns: to_ns(ticks(values[0], values[1])),
            composition_ns: to_ns(ticks(values[1], values[2])),
            total_gpu_ns: to_ns(ticks(values[0], values[2])),
        };
        *self.timing.lock().map_err(|_| {
            ImportedImageSyncError::new(
                ImportedImageSyncErrorKind::Other,
                "Vulkan video timing lock poisoned".to_string(),
            )
        })? = Some(sample);
        Ok(())
    }

    pub fn is_device_lost(&self) -> bool {
        self.device.is_device_lost()
    }
}

impl<D: VulkanDeviceContext> Drop for ImportedImageSync<D> {
    fn drop(&mut self) {
        let safe_to_destroy = if !self.acquire_submitted {
            true
        } else if self.release_submitted {
            matches!(
                unsafe { self.device.device().get_fence_status(self.release_fence) },
                Ok(true)
            )
        } else {
            false
        };
        if !safe_to_destroy {
            // Submission or renderer use may still reference every child below. Individual
            // destruction would violate Vulkan lifetime rules. Quarantine the complete device and
            // intentionally leave these raw handles for vkDestroyDevice.
            self.device.mark_device_lost();
            return;
        }
        unsafe {
            if let Some(semaphore) = self.imported_acquire.take() {
                self.device.device().destroy_semaphore(semaphore, None);
            }
            if let Some(semaphore) = self.ready_semaphore.take() {
                self.device.device().destroy_semaphore(semaphore, None);
            }
            if let Some(query) = self.timestamp_query.take() {
                self.device.device().destroy_query_pool(query.pool, None);
            }
            self.device.device().destroy_fence(self.release_fence, None);
            self.device.device().destroy_fence(self.acquire_fence, None);
            self.device
                .device()
                .destroy_command_pool(self.release_pool, None);
            self.device
                .device()
                .destroy_command_pool(self.acquire_pool, None);
        }
    }
}

fn create_command_pool<D: VulkanDeviceContext>(
    device: &D,
    label: &str,
) -> Result<vk::CommandPool, String> {
    let info = vk::CommandPoolCreateInfo::default()
        .queue_family_index(device.queue_family_index())
        .flags(vk::CommandPoolCreateFlags::TRANSIENT);
    unsafe { device.device().create_command_pool(&info, None) }
        .map_err(|result| format!("failed to create Vulkan {label} command pool: {result:?}"))
}

fn allocate_command<D: VulkanDeviceContext>(
    device: &D,
    pool: vk::CommandPool,
    label: &str,
) -> Result<vk::CommandBuffer, String> {
    let info = vk::CommandBufferAllocateInfo::default()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    unsafe { device.device().allocate_command_buffers(&info) }
        .map_err(|result| format!("failed to allocate Vulkan {label} command buffer: {result:?}"))?
        .into_iter()
        .next()
        .ok_or_else(|| format!("Vulkan returned no {label} command buffer"))
}

fn create_timestamp_query<D: VulkanDeviceContext>(
    device: &D,
) -> Result<Option<TimestampQuery>, ImportedImageSyncError> {
    let queue_properties = unsafe {
        device
            .instance()
            .get_physical_device_queue_family_properties(device.physical_device())
    };
    let valid_bits = queue_properties
        .get(usize::try_from(device.queue_family_index()).unwrap_or(usize::MAX))
        .map(|properties| properties.timestamp_valid_bits)
        .unwrap_or(0);
    if valid_bits == 0 {
        return Ok(None);
    }
    let info = vk::QueryPoolCreateInfo::default()
        .query_type(vk::QueryType::TIMESTAMP)
        .query_count(3);
    let pool = unsafe { device.device().create_query_pool(&info, None) }.map_err(|result| {
        ImportedImageSyncError::new(
            ImportedImageSyncErrorKind::Other,
            format!("failed to create Vulkan video timestamp pool: {result:?}"),
        )
    })?;
    let properties = unsafe {
        device
            .instance()
            .get_physical_device_properties(device.physical_device())
    };
    Ok(Some(TimestampQuery {
        pool,
        period_ns: f64::from(properties.limits.timestamp_period),
        valid_bits,
    }))
}

fn timestamp_delta(start: u64, end: u64, valid_bits: u32) -> u64 {
    if valid_bits >= 64 {
        end.wrapping_sub(start)
    } else {
        end.wrapping_sub(start) & ((1_u64 << valid_bits) - 1)
    }
}

fn begin_command<D: VulkanDeviceContext>(
    device: &D,
    command: vk::CommandBuffer,
) -> Result<(), String> {
    let begin =
        vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
    unsafe { device.device().begin_command_buffer(command, &begin) }
        .map_err(|result| format!("failed to begin Vulkan import command buffer: {result:?}"))
}

fn end_command<D: VulkanDeviceContext>(
    device: &D,
    command: vk::CommandBuffer,
) -> Result<(), String> {
    unsafe { device.device().end_command_buffer(command) }
        .map_err(|result| format!("failed to end Vulkan import command buffer: {result:?}"))
}

fn color_range(image: vk::Image) -> vk::ImageMemoryBarrier<'static> {
    vk::ImageMemoryBarrier::default()
        .image(image)
        .subresource_range(
            vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .base_mip_level(0)
                .level_count(1)
                .base_array_layer(0)
                .layer_count(1),
        )
}

fn record_direct_acquire<D: VulkanDeviceContext>(
    device: &D,
    command: vk::CommandBuffer,
    image: vk::Image,
) -> Result<(), String> {
    begin_command(device, command)?;
    // External producers populate the allocation in GENERAL. initialLayout=UNDEFINED at image
    // creation does not permit discarding those already-populated bytes during ownership acquire.
    let barrier = color_range(image)
        .src_access_mask(vk::AccessFlags::empty())
        .dst_access_mask(vk::AccessFlags::SHADER_READ)
        .old_layout(vk::ImageLayout::GENERAL)
        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        .src_queue_family_index(DMA_BUF_EXTERNAL_QUEUE_FAMILY)
        .dst_queue_family_index(device.queue_family_index());
    unsafe {
        device.device().cmd_pipeline_barrier(
            command,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier],
        )
    };
    end_command(device, command)
}

fn staged_output_images(output: StagedSampledImages) -> Vec<vk::Image> {
    match output {
        StagedSampledImages::Rgba { image }
        | StagedSampledImages::Bgra { image }
        | StagedSampledImages::Nv12 { image } => vec![image],
        StagedSampledImages::YuvPlanes { luma, chroma } => vec![luma, chroma],
    }
}

fn record_staged_acquire<D: VulkanDeviceContext>(
    device: &D,
    command: vk::CommandBuffer,
    plan: StagedAcquirePlan,
    timestamp_query: Option<&TimestampQuery>,
) -> Result<(), String> {
    begin_command(device, command)?;
    if let Some(query) = timestamp_query {
        unsafe {
            device
                .device()
                .cmd_reset_query_pool(command, query.pool, 0, 3);
            device.device().cmd_write_timestamp(
                command,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                query.pool,
                0,
            );
        }
    }
    let source_acquire = vk::BufferMemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::empty())
        .dst_access_mask(vk::AccessFlags::SHADER_READ)
        .src_queue_family_index(DMA_BUF_EXTERNAL_QUEUE_FAMILY)
        .dst_queue_family_index(device.queue_family_index())
        .buffer(plan.source_buffer)
        .offset(0)
        .size(plan.source_size);
    let old_layout = if plan.output_initialized {
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
    } else {
        vk::ImageLayout::UNDEFINED
    };
    let old_access = if plan.output_initialized {
        vk::AccessFlags::SHADER_READ
    } else {
        vk::AccessFlags::empty()
    };
    let output_acquires = staged_output_images(plan.output)
        .into_iter()
        .map(|image| {
            color_range(image)
                .src_access_mask(old_access)
                .dst_access_mask(vk::AccessFlags::SHADER_WRITE)
                .old_layout(old_layout)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        })
        .collect::<Vec<_>>();
    let source_stage = if plan.output_initialized {
        vk::PipelineStageFlags::FRAGMENT_SHADER
    } else {
        vk::PipelineStageFlags::TOP_OF_PIPE
    };
    unsafe {
        device.device().cmd_pipeline_barrier(
            command,
            source_stage,
            vk::PipelineStageFlags::COMPUTE_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[source_acquire],
            &output_acquires,
        );
        device
            .device()
            .cmd_bind_pipeline(command, vk::PipelineBindPoint::COMPUTE, plan.pipeline);
        device.device().cmd_bind_descriptor_sets(
            command,
            vk::PipelineBindPoint::COMPUTE,
            plan.pipeline_layout,
            0,
            &[plan.descriptor_set],
            &[],
        );
        device.device().cmd_push_constants(
            command,
            plan.pipeline_layout,
            vk::ShaderStageFlags::COMPUTE,
            0,
            plan.push_constants.as_bytes(),
        );
        device
            .device()
            .cmd_dispatch(command, plan.dispatch.0, plan.dispatch.1, 1);
    }
    let output_releases = staged_output_images(plan.output)
        .into_iter()
        .map(|image| {
            color_range(image)
                .src_access_mask(vk::AccessFlags::SHADER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ)
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        })
        .collect::<Vec<_>>();
    unsafe {
        device.device().cmd_pipeline_barrier(
            command,
            vk::PipelineStageFlags::COMPUTE_SHADER,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &output_releases,
        )
    };
    let source_release = vk::BufferMemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::SHADER_READ)
        .dst_access_mask(vk::AccessFlags::empty())
        .src_queue_family_index(device.queue_family_index())
        .dst_queue_family_index(DMA_BUF_EXTERNAL_QUEUE_FAMILY)
        .buffer(plan.source_buffer)
        .offset(0)
        .size(plan.source_size);
    unsafe {
        device.device().cmd_pipeline_barrier(
            command,
            vk::PipelineStageFlags::COMPUTE_SHADER,
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            vk::DependencyFlags::empty(),
            &[],
            &[source_release],
            &[],
        );
        if let Some(query) = timestamp_query {
            device.device().cmd_write_timestamp(
                command,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                query.pool,
                1,
            );
        }
    };
    end_command(device, command)
}

fn nv12_transfer_region(
    aspect_mask: vk::ImageAspectFlags,
    buffer_offset: u64,
    buffer_row_length: u32,
    width: u32,
    height: u32,
) -> vk::BufferImageCopy {
    vk::BufferImageCopy::default()
        .buffer_offset(buffer_offset)
        .buffer_row_length(buffer_row_length)
        .buffer_image_height(0)
        .image_subresource(
            vk::ImageSubresourceLayers::default()
                .aspect_mask(aspect_mask)
                .mip_level(0)
                .base_array_layer(0)
                .layer_count(1),
        )
        .image_offset(vk::Offset3D::default())
        .image_extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        })
}

pub(super) fn nv12_multiplanar_transfer_regions(
    plan: StagedTransferPlan,
) -> [vk::BufferImageCopy; 2] {
    [
        nv12_transfer_region(
            vk::ImageAspectFlags::PLANE_0,
            plan.planes[0].offset,
            plan.planes[0].pitch,
            plan.dimensions.0,
            plan.dimensions.1,
        ),
        nv12_transfer_region(
            vk::ImageAspectFlags::PLANE_1,
            plan.planes[1].offset,
            plan.planes[1].pitch / 2,
            plan.dimensions.0 / 2,
            plan.dimensions.1 / 2,
        ),
    ]
}

pub(super) fn nv12_separate_transfer_regions(plan: StagedTransferPlan) -> [vk::BufferImageCopy; 2] {
    [
        nv12_transfer_region(
            vk::ImageAspectFlags::COLOR,
            plan.planes[0].offset,
            plan.planes[0].pitch,
            plan.dimensions.0,
            plan.dimensions.1,
        ),
        nv12_transfer_region(
            vk::ImageAspectFlags::COLOR,
            plan.planes[1].offset,
            plan.planes[1].pitch / 2,
            plan.dimensions.0 / 2,
            plan.dimensions.1 / 2,
        ),
    ]
}

fn record_staged_transfer_acquire<D: VulkanDeviceContext>(
    device: &D,
    command: vk::CommandBuffer,
    plan: StagedTransferPlan,
    timestamp_query: Option<&TimestampQuery>,
) -> Result<(), String> {
    let output_images = match plan.output {
        StagedSampledImages::Nv12 { image } => vec![image],
        StagedSampledImages::YuvPlanes { luma, chroma } => vec![luma, chroma],
        StagedSampledImages::Rgba { .. } | StagedSampledImages::Bgra { .. } => {
            return Err("Vulkan NV12 transfer has a non-planar output".to_string());
        }
    };
    begin_command(device, command)?;
    if let Some(query) = timestamp_query {
        unsafe {
            device
                .device()
                .cmd_reset_query_pool(command, query.pool, 0, 3);
            device.device().cmd_write_timestamp(
                command,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                query.pool,
                0,
            );
        }
    }
    let source_acquire = vk::BufferMemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::empty())
        .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
        .src_queue_family_index(DMA_BUF_EXTERNAL_QUEUE_FAMILY)
        .dst_queue_family_index(device.queue_family_index())
        .buffer(plan.source_buffer)
        .offset(0)
        .size(plan.source_size);
    let old_layout = if plan.output_initialized {
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
    } else {
        vk::ImageLayout::UNDEFINED
    };
    let old_access = if plan.output_initialized {
        vk::AccessFlags::SHADER_READ
    } else {
        vk::AccessFlags::empty()
    };
    let output_acquires = output_images
        .iter()
        .copied()
        .map(|image| {
            color_range(image)
                .src_access_mask(old_access)
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .old_layout(old_layout)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        })
        .collect::<Vec<_>>();
    let source_stage = if plan.output_initialized {
        vk::PipelineStageFlags::FRAGMENT_SHADER
    } else {
        vk::PipelineStageFlags::TOP_OF_PIPE
    };
    unsafe {
        device.device().cmd_pipeline_barrier(
            command,
            source_stage,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[source_acquire],
            &output_acquires,
        );
        match plan.output {
            StagedSampledImages::Nv12 { image } => {
                device.device().cmd_copy_buffer_to_image(
                    command,
                    plan.source_buffer,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &nv12_multiplanar_transfer_regions(plan),
                );
            }
            StagedSampledImages::YuvPlanes { luma, chroma } => {
                let regions = nv12_separate_transfer_regions(plan);
                device.device().cmd_copy_buffer_to_image(
                    command,
                    plan.source_buffer,
                    luma,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &regions[0..1],
                );
                device.device().cmd_copy_buffer_to_image(
                    command,
                    plan.source_buffer,
                    chroma,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &regions[1..2],
                );
            }
            StagedSampledImages::Rgba { .. } | StagedSampledImages::Bgra { .. } => {
                unreachable!("validated transfer output")
            }
        }
    }
    let output_releases = output_images
        .into_iter()
        .map(|image| {
            color_range(image)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        })
        .collect::<Vec<_>>();
    unsafe {
        device.device().cmd_pipeline_barrier(
            command,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &output_releases,
        );
    }
    let source_release = vk::BufferMemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::TRANSFER_READ)
        .dst_access_mask(vk::AccessFlags::empty())
        .src_queue_family_index(device.queue_family_index())
        .dst_queue_family_index(DMA_BUF_EXTERNAL_QUEUE_FAMILY)
        .buffer(plan.source_buffer)
        .offset(0)
        .size(plan.source_size);
    unsafe {
        device.device().cmd_pipeline_barrier(
            command,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            vk::DependencyFlags::empty(),
            &[],
            &[source_release],
            &[],
        );
        if let Some(query) = timestamp_query {
            device.device().cmd_write_timestamp(
                command,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                query.pool,
                1,
            );
        }
    }
    end_command(device, command)
}

fn record_staged_release<D: VulkanDeviceContext>(
    device: &D,
    command: vk::CommandBuffer,
    timestamp_query: Option<&TimestampQuery>,
) -> Result<(), String> {
    begin_command(device, command)?;
    if let Some(query) = timestamp_query {
        unsafe {
            device.device().cmd_write_timestamp(
                command,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                query.pool,
                2,
            );
        }
    }
    end_command(device, command)
}

fn record_direct_release<D: VulkanDeviceContext>(
    device: &D,
    command: vk::CommandBuffer,
    image: vk::Image,
) -> Result<(), String> {
    begin_command(device, command)?;
    let barrier = color_range(image)
        .src_access_mask(vk::AccessFlags::MEMORY_READ)
        .dst_access_mask(vk::AccessFlags::empty())
        .old_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        .new_layout(vk::ImageLayout::GENERAL)
        .src_queue_family_index(device.queue_family_index())
        .dst_queue_family_index(DMA_BUF_EXTERNAL_QUEUE_FAMILY);
    unsafe {
        device.device().cmd_pipeline_barrier(
            command,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier],
        )
    };
    end_command(device, command)
}

fn import_temporary_sync_fd<D: VulkanDeviceContext>(
    device: &D,
    source_fd: i32,
    reusable: Option<vk::Semaphore>,
) -> Result<vk::Semaphore, String> {
    if source_fd < 0 {
        return Err("imported Vulkan image has an invalid sync fd".to_string());
    }
    let (semaphore, created) = match reusable {
        Some(semaphore) => (semaphore, false),
        None => (
            unsafe {
                device
                    .device()
                    .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
            }
            .map_err(|result| format!("failed to create sync-fd import semaphore: {result:?}"))?,
            true,
        ),
    };
    let duplicate = match duplicate_import_fd(source_fd) {
        Ok(duplicate) => duplicate,
        Err(error) => {
            if created {
                unsafe { device.device().destroy_semaphore(semaphore, None) };
            }
            return Err(format!(
                "failed to duplicate Vulkan acquire sync fd: {error}"
            ));
        }
    };
    let raw_duplicate = duplicate.into_raw_fd();
    let import = vk::ImportSemaphoreFdInfoKHR::default()
        .semaphore(semaphore)
        .flags(vk::SemaphoreImportFlags::TEMPORARY)
        .handle_type(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD)
        .fd(raw_duplicate);
    let loader = ash::khr::external_semaphore_fd::Device::new(device.instance(), device.device());
    match unsafe { loader.import_semaphore_fd(&import) } {
        Ok(()) => Ok(semaphore),
        Err(result) => {
            // Vulkan only consumes the descriptor after successful import.
            unsafe {
                libc::close(raw_duplicate);
                if created {
                    device.device().destroy_semaphore(semaphore, None);
                }
            }
            Err(format!(
                "failed to import Vulkan acquire sync fd: {result:?}"
            ))
        }
    }
}

pub fn validate_sync_fd_import<D: VulkanDeviceContext>(device: &D) -> Result<(), String> {
    let info = vk::PhysicalDeviceExternalSemaphoreInfo::default()
        .handle_type(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD);
    let mut properties = vk::ExternalSemaphoreProperties::default();
    unsafe {
        device
            .instance()
            .get_physical_device_external_semaphore_properties(
                device.physical_device(),
                &info,
                &mut properties,
            )
    };
    if !properties
        .external_semaphore_features
        .contains(vk::ExternalSemaphoreFeatureFlags::IMPORTABLE)
        || !properties
            .compatible_handle_types
            .contains(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD)
    {
        return Err("Vulkan device cannot import SYNC_FD acquire semaphores".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dma_buf_ownership_uses_core_external_queue_family() {
        assert_eq!(DMA_BUF_EXTERNAL_QUEUE_FAMILY, vk::QUEUE_FAMILY_EXTERNAL);
        assert_ne!(DMA_BUF_EXTERNAL_QUEUE_FAMILY, vk::QUEUE_FAMILY_FOREIGN_EXT);
    }

    #[test]
    fn timestamp_delta_handles_valid_bit_wrap_without_waiting() {
        assert_eq!(timestamp_delta(10, 25, 64), 15);
        assert_eq!(timestamp_delta(250, 5, 8), 11);
    }
}
