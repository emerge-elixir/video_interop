//! Driver-free command tracing of the real sync owner/recorders. No Vulkan loader/GPU needed.
use super::*;
use ash::vk::Handle;
use std::{
    cell::RefCell,
    ffi::{CStr, c_void},
    sync::atomic::AtomicUsize,
};

#[derive(Default, Clone, Debug)]
struct Trace {
    query_allocations: usize,
    query_reads: usize,
    read_result: vk::Result,
    query_resets: usize,
    writes: Vec<u32>,
    barriers: usize,
    copies: usize,
    begins: usize,
    ends: usize,
    query_result: vk::Result,
    valid_bits: u32,
    period: f32,
    fence_complete: bool,
}
thread_local! { static TRACE: RefCell<Trace> = RefCell::new(Trace::default()); }
fn trace() -> Trace {
    TRACE.with(|t| t.borrow().clone())
}
fn reset() {
    TRACE.with(|t| {
        *t.borrow_mut() = Trace {
            valid_bits: 64,
            period: 1.0,
            fence_complete: true,
            ..Trace::default()
        }
    });
}

// These typed dispatch functions implement only the Vulkan calls exercised below. All handles
// are test tokens, never passed to a real driver. Ash panics on any unexpected missing function.
unsafe extern "system" fn create_pool(
    _: vk::Device,
    _: *const vk::CommandPoolCreateInfo<'_>,
    _: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::CommandPool,
) -> vk::Result {
    unsafe {
        *out = vk::CommandPool::from_raw(1);
    }
    vk::Result::SUCCESS
}
unsafe extern "system" fn allocate(
    _: vk::Device,
    _: *const vk::CommandBufferAllocateInfo<'_>,
    out: *mut vk::CommandBuffer,
) -> vk::Result {
    unsafe {
        *out = vk::CommandBuffer::from_raw(2);
    }
    vk::Result::SUCCESS
}
unsafe extern "system" fn create_fence(
    _: vk::Device,
    _: *const vk::FenceCreateInfo<'_>,
    _: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Fence,
) -> vk::Result {
    unsafe {
        *out = vk::Fence::from_raw(3);
    }
    vk::Result::SUCCESS
}
unsafe extern "system" fn create_query(
    _: vk::Device,
    _: *const vk::QueryPoolCreateInfo<'_>,
    _: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::QueryPool,
) -> vk::Result {
    TRACE.with(|t| {
        let mut t = t.borrow_mut();
        t.query_allocations += 1;
        unsafe {
            *out = vk::QueryPool::from_raw(4);
        }
        t.query_result
    })
}
unsafe extern "system" fn destroy_pool(
    _: vk::Device,
    _: vk::CommandPool,
    _: *const vk::AllocationCallbacks<'_>,
) {
}
unsafe extern "system" fn destroy_fence(
    _: vk::Device,
    _: vk::Fence,
    _: *const vk::AllocationCallbacks<'_>,
) {
}
unsafe extern "system" fn destroy_query(
    _: vk::Device,
    _: vk::QueryPool,
    _: *const vk::AllocationCallbacks<'_>,
) {
}
unsafe extern "system" fn reset_pool(
    _: vk::Device,
    _: vk::CommandPool,
    _: vk::CommandPoolResetFlags,
) -> vk::Result {
    vk::Result::SUCCESS
}
unsafe extern "system" fn begin(
    _: vk::CommandBuffer,
    _: *const vk::CommandBufferBeginInfo<'_>,
) -> vk::Result {
    TRACE.with(|t| t.borrow_mut().begins += 1);
    vk::Result::SUCCESS
}
unsafe extern "system" fn end(_: vk::CommandBuffer) -> vk::Result {
    TRACE.with(|t| t.borrow_mut().ends += 1);
    vk::Result::SUCCESS
}
unsafe extern "system" fn query_reset(_: vk::CommandBuffer, _: vk::QueryPool, _: u32, _: u32) {
    TRACE.with(|t| t.borrow_mut().query_resets += 1);
}
unsafe extern "system" fn timestamp(
    _: vk::CommandBuffer,
    _: vk::PipelineStageFlags,
    _: vk::QueryPool,
    index: u32,
) {
    TRACE.with(|t| t.borrow_mut().writes.push(index));
}
unsafe extern "system" fn barrier(
    _: vk::CommandBuffer,
    _: vk::PipelineStageFlags,
    _: vk::PipelineStageFlags,
    _: vk::DependencyFlags,
    _: u32,
    _: *const vk::MemoryBarrier<'_>,
    _: u32,
    _: *const vk::BufferMemoryBarrier<'_>,
    _: u32,
    _: *const vk::ImageMemoryBarrier<'_>,
) {
    TRACE.with(|t| t.borrow_mut().barriers += 1);
}
unsafe extern "system" fn copy(
    _: vk::CommandBuffer,
    _: vk::Buffer,
    _: vk::Image,
    _: vk::ImageLayout,
    _: u32,
    _: *const vk::BufferImageCopy,
) {
    TRACE.with(|t| t.borrow_mut().copies += 1);
}
unsafe extern "system" fn families(
    _: vk::PhysicalDevice,
    count: *mut u32,
    out: *mut vk::QueueFamilyProperties,
) {
    unsafe {
        *count = 1;
        if !out.is_null() {
            *out = vk::QueueFamilyProperties {
                timestamp_valid_bits: trace().valid_bits,
                ..Default::default()
            };
        }
    }
}
unsafe extern "system" fn properties(
    _: vk::PhysicalDevice,
    out: *mut vk::PhysicalDeviceProperties,
) {
    let mut p = vk::PhysicalDeviceProperties::default();
    p.limits.timestamp_period = trace().period;
    unsafe {
        *out = p;
    }
}
unsafe extern "system" fn fence_status(_: vk::Device, _: vk::Fence) -> vk::Result {
    if trace().fence_complete {
        vk::Result::SUCCESS
    } else {
        vk::Result::NOT_READY
    }
}
unsafe extern "system" fn read_query(
    _: vk::Device,
    _: vk::QueryPool,
    _: u32,
    _: u32,
    _: usize,
    out: *mut c_void,
    _: vk::DeviceSize,
    _: vk::QueryResultFlags,
) -> vk::Result {
    TRACE.with(|t| {
        let mut t = t.borrow_mut();
        t.query_reads += 1;
        if t.read_result == vk::Result::SUCCESS {
            unsafe {
                std::ptr::copy_nonoverlapping([1_u64, 11, 31].as_ptr(), out.cast::<u64>(), 3);
            }
        }
        t.read_result
    })
}
fn load(name: &CStr) -> *const c_void {
    match name.to_bytes() {
        b"vkCreateCommandPool" => create_pool as *const c_void,
        b"vkAllocateCommandBuffers" => allocate as *const c_void,
        b"vkCreateFence" => create_fence as *const c_void,
        b"vkCreateQueryPool" => create_query as *const c_void,
        b"vkGetQueryPoolResults" => read_query as *const c_void,
        b"vkDestroyCommandPool" => destroy_pool as *const c_void,
        b"vkDestroyFence" => destroy_fence as *const c_void,
        b"vkDestroyQueryPool" => destroy_query as *const c_void,
        b"vkResetCommandPool" => reset_pool as *const c_void,
        b"vkBeginCommandBuffer" => begin as *const c_void,
        b"vkEndCommandBuffer" => end as *const c_void,
        b"vkCmdResetQueryPool" => query_reset as *const c_void,
        b"vkCmdWriteTimestamp" => timestamp as *const c_void,
        b"vkCmdPipelineBarrier" => barrier as *const c_void,
        b"vkCmdCopyBufferToImage" => copy as *const c_void,
        b"vkGetPhysicalDeviceQueueFamilyProperties" => families as *const c_void,
        b"vkGetPhysicalDeviceProperties" => properties as *const c_void,
        b"vkGetFenceStatus" => fence_status as *const c_void,
        _ => std::ptr::null(),
    }
}
struct Context {
    instance: ash::Instance,
    device: ash::Device,
    enabled: bool,
    sample: AtomicBool,
    choices: AtomicUsize,
    lost: AtomicBool,
    status: Mutex<String>,
}
impl Context {
    fn new(enabled: bool) -> Arc<Self> {
        reset();
        Arc::new(Self {
            instance: unsafe { ash::Instance::load_with(load, vk::Instance::from_raw(1)) },
            device: unsafe { ash::Device::load_with(load, vk::Device::from_raw(1)) },
            enabled,
            sample: AtomicBool::new(true),
            choices: AtomicUsize::new(0),
            lost: AtomicBool::new(false),
            status: Mutex::new(String::new()),
        })
    }
}
impl VulkanDeviceContext for Context {
    fn instance(&self) -> &ash::Instance {
        &self.instance
    }
    fn device(&self) -> &ash::Device {
        &self.device
    }
    fn physical_device(&self) -> vk::PhysicalDevice {
        vk::PhysicalDevice::from_raw(1)
    }
    fn queue(&self) -> vk::Queue {
        vk::Queue::from_raw(1)
    }
    fn queue_family_index(&self) -> u32 {
        0
    }
    fn video_gpu_timing_enabled(&self) -> bool {
        self.enabled
    }
    fn sample_video_gpu_timing(&self) -> bool {
        self.choices.fetch_add(1, Ordering::Relaxed);
        self.sample.load(Ordering::Relaxed)
    }
    fn report_video_gpu_timing(&self, status: &str) {
        *self.status.lock().unwrap() = status.into();
    }
    unsafe fn submit_video_queue(
        &self,
        _: &[vk::SubmitInfo<'_>],
        _: vk::Fence,
    ) -> Result<(), vk::Result> {
        panic!("unexpected queue submit")
    }
    fn mark_device_lost(&self) {
        self.lost.store(true, Ordering::Relaxed);
    }
    fn is_device_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }
}
fn transfer() -> AcquirePlan {
    use super::super::ImportedPlane;
    AcquirePlan::StagedTransfer(StagedTransferPlan {
        source_buffer: vk::Buffer::from_raw(5),
        source_size: 24,
        output: StagedSampledImages::YuvPlanes {
            luma: vk::Image::from_raw(6),
            chroma: vk::Image::from_raw(7),
        },
        output_initialized: false,
        dimensions: (4, 4),
        planes: [
            ImportedPlane {
                offset: 0,
                pitch: 4,
            },
            ImportedPlane {
                offset: 16,
                pitch: 4,
            },
        ],
    })
}
#[test]
fn off_skips_all_query_work_but_keeps_copy_barriers_and_empty_release_commands() {
    let ctx = Context::new(false);
    let mut lane = ImportedImageSync::new(Arc::clone(&ctx)).unwrap();
    assert!(!lane.has_gpu_timing_resources());
    lane.record_acquire(transfer()).unwrap();
    record_staged_release(
        ctx.as_ref(),
        lane.release_command,
        lane.active_timestamp_query(),
    )
    .unwrap();
    lane.collect_timing().unwrap();
    assert_eq!(trace().query_reads, 0);
    assert!(lane.take_timing().is_none());
    assert_eq!(trace().query_allocations, 0);
    assert_eq!(trace().query_resets, 0);
    assert!(trace().writes.is_empty());
    assert_eq!(trace().barriers, 3);
    assert_eq!(trace().copies, 2);
    assert_eq!((trace().begins, trace().ends), (2, 2));
    assert_eq!(ctx.choices.load(Ordering::Relaxed), 0);
}
#[test]
fn sparse_decision_survives_release_and_is_reselected_only_after_proven_reuse() {
    let ctx = Context::new(true);
    let mut lane = ImportedImageSync::new(Arc::clone(&ctx)).unwrap();
    for sampled in [true, false, true, false] {
        ctx.sample.store(sampled, Ordering::Relaxed);
        lane.record_acquire(transfer()).unwrap();
        ctx.sample.store(!sampled, Ordering::Relaxed); // Must not alter this frame's release.
        record_staged_release(
            ctx.as_ref(),
            lane.release_command,
            lane.active_timestamp_query(),
        )
        .unwrap();
        lane.release_submitted = true;
        TRACE.with(|t| t.borrow_mut().fence_complete = false);
        assert!(lane.reset_for_reuse().is_err());
        TRACE.with(|t| t.borrow_mut().fence_complete = true);
        lane.reset_for_reuse().unwrap();
        assert!(!lane.timing_active);
    }
    assert_eq!(trace().query_allocations, 1);
    assert_eq!(trace().writes, vec![0, 1, 2, 0, 1, 2]);
    assert_eq!(trace().query_resets, 2);
    assert_eq!(trace().copies, 8);
    assert_eq!(trace().barriers, 12);
    assert_eq!(ctx.choices.load(Ordering::Relaxed), 4);
}
#[test]
fn unsupported_timestamps_do_not_block_sync_owner_creation() {
    for (bits, period) in [(0, 1.0), (65, 1.0), (64, 0.0), (64, f32::NAN)] {
        let ctx = Context::new(true);
        TRACE.with(|t| {
            let mut t = t.borrow_mut();
            t.valid_bits = bits;
            t.period = period;
        });
        let lane = ImportedImageSync::new(Arc::clone(&ctx)).unwrap();
        assert!(!lane.has_gpu_timing_resources());
        assert_eq!(trace().query_allocations, 0);
        assert_eq!(*ctx.status.lock().unwrap(), "unsupported");
    }
}
#[test]
fn optional_pool_oom_is_not_device_loss_and_device_loss_is_not_optional_success() {
    for result in [
        vk::Result::ERROR_OUT_OF_HOST_MEMORY,
        vk::Result::ERROR_OUT_OF_DEVICE_MEMORY,
        vk::Result::ERROR_DEVICE_LOST,
    ] {
        let ctx = Context::new(true);
        TRACE.with(|t| t.borrow_mut().query_result = result);
        let lane = ImportedImageSync::new(Arc::clone(&ctx));
        if result == vk::Result::ERROR_DEVICE_LOST {
            assert!(lane.err().unwrap().is_device_lost());
            assert!(ctx.is_device_lost());
        } else {
            assert!(!lane.unwrap().has_gpu_timing_resources());
            assert!(!ctx.is_device_lost());
            assert_eq!(*ctx.status.lock().unwrap(), "failed");
        }
    }
}

#[test]
fn query_results_follow_release_completion_and_are_collected_once() {
    for result in [
        vk::Result::SUCCESS,
        vk::Result::NOT_READY,
        vk::Result::ERROR_OUT_OF_HOST_MEMORY,
        vk::Result::ERROR_DEVICE_LOST,
    ] {
        let ctx = Context::new(true);
        let mut lane = ImportedImageSync::new(Arc::clone(&ctx)).unwrap();
        lane.record_acquire(transfer()).unwrap();
        lane.release_submitted = true;
        TRACE.with(|t| {
            let mut t = t.borrow_mut();
            t.fence_complete = false;
            t.read_result = result;
        });
        assert!(!lane.release_complete().unwrap());
        assert_eq!(trace().query_reads, 0);
        TRACE.with(|t| t.borrow_mut().fence_complete = true);
        if result == vk::Result::ERROR_DEVICE_LOST {
            assert!(lane.release_complete().unwrap_err().is_device_lost());
            assert!(ctx.is_device_lost());
        } else {
            assert!(lane.release_complete().unwrap());
            assert!(lane.release_complete().unwrap());
            assert_eq!(trace().query_reads, 1);
            if result == vk::Result::SUCCESS {
                assert_eq!(
                    lane.take_timing(),
                    Some(VulkanVideoTiming {
                        conversion_ns: 10,
                        composition_ns: 20,
                        total_gpu_ns: 30
                    })
                );
            } else {
                assert_eq!(*ctx.status.lock().unwrap(), "failed");
                assert!(lane.take_timing().is_none());
            }
        }
    }
}
