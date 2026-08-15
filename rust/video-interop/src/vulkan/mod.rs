//! Optional Vulkan DMA-BUF import adapter.
//!
//! This module owns generic DMA-BUF validation, external-memory import, active-device
//! capability queries, and acquire/release synchronization. A renderer supplies an already
//! selected Vulkan device and may wrap [`ImportedDmaBufImage::image`] in its own rendering API.

mod sync;

use std::{
    collections::HashMap,
    os::fd::{IntoRawFd, OwnedFd},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use ash::{Device, Instance, vk};

use crate::{
    ChromaLocation, ColorRange, Colorimetry, Matrix, Primaries, Transfer, duplicate_fd_cloexec,
};

pub use sync::{
    DMA_BUF_EXTERNAL_QUEUE_FAMILY, ImportedImageSync, ImportedImageSyncError,
    ImportedImageSyncErrorKind, VulkanVideoTiming, validate_sync_fd_import,
};

pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;
pub const IMPORTED_RGBA_FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;
pub const IMPORTED_SCANOUT_BGRA_FORMAT: vk::Format = vk::Format::B8G8R8A8_UNORM;
pub const IMPORTED_NV12_FORMAT: vk::Format = vk::Format::G8_B8R8_2PLANE_420_UNORM;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PackedImageFormat {
    Rgba8888,
    Bgra8888,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PackedImageImportStrategy {
    /// The producer allocation is directly sampled as a Vulkan image.
    DirectSampledImage,
    /// Linear packed pixels are imported as a uniform texel buffer and copied by compute into an
    /// optimal BGRA image. This remains an ordinary renderer image at every paint-layer position.
    LinearBufferToOptimalBgra,
}

impl PackedImageFormat {
    fn vk_format(self) -> vk::Format {
        match self {
            Self::Rgba8888 => IMPORTED_RGBA_FORMAT,
            Self::Bgra8888 => IMPORTED_SCANOUT_BGRA_FORMAT,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Rgba8888 => "R8G8B8A8",
            Self::Bgra8888 => "B8G8R8A8",
        }
    }
}

const DIRECT_PACKED_USAGE: vk::ImageUsageFlags = vk::ImageUsageFlags::from_raw(
    vk::ImageUsageFlags::SAMPLED.as_raw()
        | vk::ImageUsageFlags::COLOR_ATTACHMENT.as_raw()
        | vk::ImageUsageFlags::TRANSFER_SRC.as_raw()
        | vk::ImageUsageFlags::TRANSFER_DST.as_raw(),
);
const DIRECT_NV12_USAGE: vk::ImageUsageFlags = vk::ImageUsageFlags::SAMPLED;
// Ganesh rejects wrapped Vulkan textures unless both transfer bits are declared, even when it
// only samples them. These are importer-owned optimal output images with Vulkan-sized local
// allocations, not camera DMA-BUFs, so declaring transfer compatibility cannot expose the
// producer allocation to V3D TFU read-ahead.
const STAGED_NV12_OUTPUT_USAGE: vk::ImageUsageFlags = vk::ImageUsageFlags::from_raw(
    vk::ImageUsageFlags::SAMPLED.as_raw()
        | vk::ImageUsageFlags::STORAGE.as_raw()
        | vk::ImageUsageFlags::TRANSFER_SRC.as_raw()
        | vk::ImageUsageFlags::TRANSFER_DST.as_raw(),
);
// The optimal multi-planar destination is importer-owned and therefore may truthfully declare
// Ganesh's transfer compatibility in addition to the transfer-destination operation that fills it.
const TRANSFER_NV12_OUTPUT_USAGE: vk::ImageUsageFlags = vk::ImageUsageFlags::from_raw(
    vk::ImageUsageFlags::SAMPLED.as_raw()
        | vk::ImageUsageFlags::TRANSFER_SRC.as_raw()
        | vk::ImageUsageFlags::TRANSFER_DST.as_raw(),
);
const STAGED_NV12_SOURCE_USAGE: vk::BufferUsageFlags = vk::BufferUsageFlags::UNIFORM_TEXEL_BUFFER;
const TRANSFER_NV12_SOURCE_USAGE: vk::BufferUsageFlags = vk::BufferUsageFlags::TRANSFER_SRC;
const STAGED_NV12_SOURCE_TEXEL_FORMAT: vk::Format = vk::Format::R32_UINT;
const STAGED_NV12_LUMA_FORMAT: vk::Format = vk::Format::R8_UNORM;
const STAGED_NV12_CHROMA_FORMAT: vk::Format = vk::Format::R8G8_UNORM;
const STAGED_PACKED_SOURCE_USAGE: vk::BufferUsageFlags = vk::BufferUsageFlags::UNIFORM_TEXEL_BUFFER;
const STAGED_PACKED_SOURCE_TEXEL_FORMAT: vk::Format = vk::Format::R32_UINT;
const STAGED_PACKED_STORAGE_VIEW_FORMAT: vk::Format = vk::Format::R32_UINT;
const STAGED_PACKED_OUTPUT_USAGE: vk::ImageUsageFlags = STAGED_NV12_OUTPUT_USAGE;
const DEFAULT_NV12_SOURCE_CACHE_ENTRIES: usize = 32;
const DEFAULT_NV12_OUTPUT_SLOTS: usize = 8;
const DEFAULT_PACKED_SOURCE_CACHE_ENTRIES: usize = 32;
const DEFAULT_PACKED_OUTPUT_SLOTS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VulkanImportPoolLimits {
    pub nv12_source_cache_entries: usize,
    pub nv12_output_slots: usize,
    pub packed_source_cache_entries: usize,
    pub packed_output_slots: usize,
}

impl Default for VulkanImportPoolLimits {
    fn default() -> Self {
        Self {
            nv12_source_cache_entries: DEFAULT_NV12_SOURCE_CACHE_ENTRIES,
            nv12_output_slots: DEFAULT_NV12_OUTPUT_SLOTS,
            packed_source_cache_entries: DEFAULT_PACKED_SOURCE_CACHE_ENTRIES,
            packed_output_slots: DEFAULT_PACKED_OUTPUT_SLOTS,
        }
    }
}

/// The Vulkan handles required by the import adapter.
///
/// Device creation, physical-device selection, queue policy, presentation, and renderer state stay
/// with the caller. Implementations must keep every returned handle alive for the lifetime of any
/// importer or imported image.
pub trait VulkanDeviceContext: Send + Sync + 'static {
    fn instance(&self) -> &Instance;
    fn physical_device(&self) -> vk::PhysicalDevice;
    fn device(&self) -> &Device;
    fn queue(&self) -> vk::Queue;
    fn queue_family_index(&self) -> u32;
    fn mark_device_lost(&self);
    fn is_device_lost(&self) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImportedPlane {
    pub offset: u64,
    pub pitch: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PackedImageImport {
    pub stream_incarnation: u64,
    pub dimensions: (u32, u32),
    pub source_fd: i32,
    pub source_size: u64,
    pub modifier: u64,
    pub plane: ImportedPlane,
    pub format: PackedImageFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Nv12Plane {
    pub object_index: u32,
    pub offset: u64,
    pub pitch: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Nv12SharedObjectLayout {
    pub modifier: u64,
    pub object_size: u64,
    pub planes: [ImportedPlane; 2],
}

impl Nv12SharedObjectLayout {
    pub fn frame_topology(self, dimensions: (u32, u32)) -> Nv12FrameTopology {
        Nv12FrameTopology {
            dimensions,
            object_count: 1,
            object_size: self.object_size,
            plane_count: 2,
            planes: self.planes.map(|plane| Nv12Plane {
                object_index: 0,
                offset: plane.offset,
                pitch: plane.pitch,
            }),
            modifier: self.modifier,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Nv12FrameTopology {
    pub dimensions: (u32, u32),
    pub object_count: u32,
    pub object_size: u64,
    pub plane_count: u32,
    pub planes: [Nv12Plane; 2],
    pub modifier: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Nv12ImportStrategy {
    /// The DMA-BUF allocation is directly sampled as a multiplanar Vulkan image.
    DirectSampledImage,
    /// Linear NV12 is imported as a transfer-source buffer and copied plane-for-plane into one
    /// optimal multi-planar NV12 image. Vulkan sampler YCbCr conversion remains deferred to the
    /// renderer, so no RGB intermediate is produced.
    LinearBufferToOptimalNv12,
    /// Linear NV12 is imported as a transfer-source buffer and copied into separate optimal
    /// `R8_UNORM` and `R8G8_UNORM` images. This preserves the renderer's exact YUV shader when
    /// hardware sampler YCbCr conversion cannot provide the required filtering.
    LinearBufferToOptimalYuvPlanes,
    /// Linear NV12 is imported as a uniform texel buffer and copied by compute into optimal
    /// `R8_UNORM` and `R8G8_UNORM` images. Conversion remains deferred to the renderer.
    LinearBufferToYuvPlanes,
    /// Linear NV12 is imported as a uniform texel buffer and converted by compute into an
    /// optimal-tiled RGBA image.
    LinearBufferToRgba,
}

/// Renderer policy for linear NV12 when direct sampling is unavailable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Nv12StagingPreference {
    /// Prefer a plane-for-plane transfer into optimal multi-planar NV12, then separate optimal
    /// Y/UV planes when exact hardware YCbCr filtering is unavailable, then compute Y/UV planes.
    /// Use exact compute RGBA only when no planar path is available.
    #[default]
    PreferPlanar,
    /// Require the established compute Y/UV-plane path. This remains the explicit rollback and
    /// qualification mode while `auto` evaluates the faster multi-planar transfer path.
    RequirePlanar,
    /// Require exact compute RGBA output; never silently change the benchmarked path.
    RequireRgba,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Nv12AllocationBindingRecipe {
    DirectSharedImage,
    LinearBufferToOptimalNv12,
    LinearBufferToOptimalYuvPlanes,
    LinearBufferToYuvPlanes,
    LinearBufferToRgba,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum YcbcrModel {
    Bt601,
    Bt709,
    Bt2020,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum YcbcrRange {
    Narrow,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum YcbcrOffset {
    CositedEven,
    Midpoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Nv12Conversion {
    pub model: YcbcrModel,
    pub range: YcbcrRange,
    pub x_offset: YcbcrOffset,
    pub y_offset: YcbcrOffset,
}

impl Nv12Conversion {
    pub fn required_direct_features(self) -> vk::FormatFeatureFlags {
        let siting = match (self.x_offset, self.y_offset) {
            (YcbcrOffset::Midpoint, YcbcrOffset::Midpoint) => {
                vk::FormatFeatureFlags::MIDPOINT_CHROMA_SAMPLES
            }
            (YcbcrOffset::CositedEven, YcbcrOffset::CositedEven) => {
                vk::FormatFeatureFlags::COSITED_CHROMA_SAMPLES
            }
            _ => {
                vk::FormatFeatureFlags::MIDPOINT_CHROMA_SAMPLES
                    | vk::FormatFeatureFlags::COSITED_CHROMA_SAMPLES
            }
        };
        vk::FormatFeatureFlags::SAMPLED_IMAGE
            | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR
            | vk::FormatFeatureFlags::SAMPLED_IMAGE_YCBCR_CONVERSION_LINEAR_FILTER
            | siting
    }
}

pub fn map_nv12_colorimetry(color: Colorimetry) -> Result<Nv12Conversion, String> {
    let model = match (color.primaries, color.transfer, color.matrix) {
        (Primaries::Bt709, Transfer::Bt709, Matrix::Bt709) => YcbcrModel::Bt709,
        (Primaries::Unspecified, _, _)
        | (_, Transfer::Unspecified, _)
        | (_, _, Matrix::Unspecified) => {
            return Err(
                "Vulkan NV12 requires explicit primaries, transfer, and matrix".to_string(),
            );
        }
        contract => {
            return Err(format!(
                "Vulkan NV12 color contract {contract:?} is not identical to the BT.709 output contract"
            ));
        }
    };
    let range = match color.range {
        ColorRange::Limited => YcbcrRange::Narrow,
        ColorRange::Full => YcbcrRange::Full,
        ColorRange::Unspecified => {
            return Err("Vulkan NV12 requires an explicit color range".to_string());
        }
    };
    let (x_offset, y_offset) = match color.chroma_location {
        ChromaLocation::Left => (YcbcrOffset::CositedEven, YcbcrOffset::Midpoint),
        ChromaLocation::Center => (YcbcrOffset::Midpoint, YcbcrOffset::Midpoint),
        ChromaLocation::TopLeft => (YcbcrOffset::CositedEven, YcbcrOffset::CositedEven),
        ChromaLocation::Top => (YcbcrOffset::Midpoint, YcbcrOffset::CositedEven),
        ChromaLocation::Unspecified => {
            return Err("Vulkan NV12 requires an explicit chroma location".to_string());
        }
        ChromaLocation::BottomLeft | ChromaLocation::Bottom => {
            return Err("bottom-sited chroma is unsupported for Vulkan NV12".to_string());
        }
    };
    Ok(Nv12Conversion {
        model,
        range,
        x_offset,
        y_offset,
    })
}

pub fn validate_nv12_shared_object_topology(
    dimensions: (u32, u32),
    object_sizes: &[u64],
    object_modifiers: &[Option<u64>],
    planes: &[Nv12Plane],
) -> Result<Nv12SharedObjectLayout, String> {
    let (width, height) = dimensions;
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(format!(
            "Vulkan NV12 requires positive even coded dimensions, got {width}x{height}"
        ));
    }
    if object_sizes.len() != 1 || object_modifiers.len() != 1 || planes.len() != 2 {
        return Err(format!(
            "Vulkan NV12 shared-object import requires exactly one object and two planes, got {} object size(s), {} modifier(s), and {} plane(s)",
            object_sizes.len(),
            object_modifiers.len(),
            planes.len()
        ));
    }
    if planes.iter().any(|plane| plane.object_index != 0) {
        return Err("Vulkan NV12 shared-object planes must both reference object zero".to_string());
    }
    let modifier = object_modifiers[0].ok_or_else(|| {
        "Vulkan NV12 requires an explicit DRM modifier; implicit modifier is unsupported"
            .to_string()
    })?;
    let layout = Nv12SharedObjectLayout {
        modifier,
        object_size: object_sizes[0],
        planes: [
            ImportedPlane {
                offset: planes[0].offset,
                pitch: planes[0].pitch,
            },
            ImportedPlane {
                offset: planes[1].offset,
                pitch: planes[1].pitch,
            },
        ],
    };
    validate_nv12_shared_layout(dimensions, layout)?;
    Ok(layout)
}

fn validate_nv12_shared_layout(
    dimensions: (u32, u32),
    layout: Nv12SharedObjectLayout,
) -> Result<(), String> {
    let (width, height) = dimensions;
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(format!(
            "Vulkan NV12 requires positive even coded dimensions, got {width}x{height}"
        ));
    }
    if layout.planes.iter().any(|plane| plane.pitch < width) {
        return Err(format!(
            "Vulkan NV12 plane pitch must be at least coded width {width}"
        ));
    }
    if !layout.planes[1].pitch.is_multiple_of(2) {
        return Err(
            "Vulkan NV12 chroma pitch must be divisible by two bytes per texel".to_string(),
        );
    }
    let plane_end = |plane: ImportedPlane, rows: u32| {
        plane
            .offset
            .checked_add(u64::from(plane.pitch) * u64::from(rows.saturating_sub(1)))
            .and_then(|end| end.checked_add(u64::from(width)))
            .ok_or_else(|| "Vulkan NV12 plane extent overflow".to_string())
    };
    let luma_end = plane_end(layout.planes[0], height)?;
    let chroma_end = plane_end(layout.planes[1], height / 2)?;
    if luma_end > layout.object_size || chroma_end > layout.object_size {
        return Err(format!(
            "Vulkan NV12 planes exceed DMA-BUF object size {} (luma_end={luma_end}, chroma_end={chroma_end})",
            layout.object_size
        ));
    }
    let disjoint_ranges =
        luma_end <= layout.planes[1].offset || chroma_end <= layout.planes[0].offset;
    if !disjoint_ranges {
        return Err("Vulkan NV12 luma and chroma plane byte ranges overlap".to_string());
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub struct Nv12ModifierCapability {
    pub modifier: u64,
    pub strategy: Nv12ImportStrategy,
    /// Driver-advertised DRM memory-plane count. It remains diagnostic truth even when the staged
    /// buffer strategy avoids relying on a known-bad multiplanar image report.
    pub modifier_plane_count: u32,
    pub source_tiling_features: vk::FormatFeatureFlags,
    pub sampled_tiling_features: vk::FormatFeatureFlags,
    pub external_features: vk::ExternalMemoryFeatureFlags,
    pub compatible_handle_types: vk::ExternalMemoryHandleTypeFlags,
    pub max_extent: vk::Extent3D,
}

impl Nv12ModifierCapability {
    pub fn allocation_recipe(self) -> Nv12AllocationBindingRecipe {
        match self.strategy {
            Nv12ImportStrategy::DirectSampledImage => {
                Nv12AllocationBindingRecipe::DirectSharedImage
            }
            Nv12ImportStrategy::LinearBufferToOptimalNv12 => {
                Nv12AllocationBindingRecipe::LinearBufferToOptimalNv12
            }
            Nv12ImportStrategy::LinearBufferToOptimalYuvPlanes => {
                Nv12AllocationBindingRecipe::LinearBufferToOptimalYuvPlanes
            }
            Nv12ImportStrategy::LinearBufferToYuvPlanes => {
                Nv12AllocationBindingRecipe::LinearBufferToYuvPlanes
            }
            Nv12ImportStrategy::LinearBufferToRgba => {
                Nv12AllocationBindingRecipe::LinearBufferToRgba
            }
        }
    }
}

pub fn validate_nv12_modifier_capability(
    capability: Nv12ModifierCapability,
    dimensions: (u32, u32),
    conversion: Nv12Conversion,
) -> Result<(), String> {
    if dimensions.0 == 0 || dimensions.1 == 0 {
        return Err("Vulkan NV12 dimensions must be non-zero".to_string());
    }
    if dimensions.0 > capability.max_extent.width || dimensions.1 > capability.max_extent.height {
        return Err(format!(
            "Vulkan NV12 dimensions {}x{} exceed import limit {}x{}",
            dimensions.0, dimensions.1, capability.max_extent.width, capability.max_extent.height
        ));
    }
    if !capability
        .external_features
        .contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE)
        || !capability
            .compatible_handle_types
            .contains(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
    {
        return Err(format!(
            "Vulkan NV12 modifier {:#018x} is not DMA-BUF importable",
            capability.modifier
        ));
    }
    match capability.strategy {
        Nv12ImportStrategy::DirectSampledImage => {
            if capability.modifier_plane_count != 2 {
                return Err(format!(
                    "Vulkan NV12 modifier {:#018x} reports {} memory plane(s), expected exactly two for direct image import",
                    capability.modifier, capability.modifier_plane_count
                ));
            }
            let required = conversion.required_direct_features();
            if !capability.sampled_tiling_features.contains(required) {
                return Err(format!(
                    "Vulkan NV12 modifier {:#018x} lacks required direct sampling/filter/siting features 0x{:x} (available=0x{:x})",
                    capability.modifier,
                    required.as_raw(),
                    capability.sampled_tiling_features.as_raw()
                ));
            }
        }
        Nv12ImportStrategy::LinearBufferToOptimalNv12 => {
            if capability.modifier != DRM_FORMAT_MOD_LINEAR {
                return Err(format!(
                    "Vulkan buffer-to-optimal NV12 transfer only supports linear modifier 0, got {:#018x}",
                    capability.modifier
                ));
            }
            let required = conversion.required_direct_features()
                | vk::FormatFeatureFlags::TRANSFER_SRC
                | vk::FormatFeatureFlags::TRANSFER_DST;
            if !capability.sampled_tiling_features.contains(required) {
                return Err(format!(
                    "Vulkan optimal multi-planar NV12 output lacks required transfer/sampling/filter/siting features 0x{:x} (available=0x{:x})",
                    required.as_raw(),
                    capability.sampled_tiling_features.as_raw()
                ));
            }
        }
        Nv12ImportStrategy::LinearBufferToOptimalYuvPlanes => {
            if capability.modifier != DRM_FORMAT_MOD_LINEAR {
                return Err(format!(
                    "Vulkan buffer-to-optimal YUV-plane transfer only supports linear modifier 0, got {:#018x}",
                    capability.modifier
                ));
            }
            if conversion.model != YcbcrModel::Bt709 {
                return Err(
                    "Vulkan separate-plane NV12 transfer currently requires BT.709".to_string(),
                );
            }
            let required = vk::FormatFeatureFlags::SAMPLED_IMAGE
                | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR
                | vk::FormatFeatureFlags::TRANSFER_SRC
                | vk::FormatFeatureFlags::TRANSFER_DST;
            if !capability.sampled_tiling_features.contains(required) {
                return Err(format!(
                    "Vulkan optimal YUV-plane transfer output lacks required transfer/sampling/filter features 0x{:x} (available=0x{:x})",
                    required.as_raw(),
                    capability.sampled_tiling_features.as_raw()
                ));
            }
        }
        Nv12ImportStrategy::LinearBufferToYuvPlanes | Nv12ImportStrategy::LinearBufferToRgba => {
            if capability.modifier != DRM_FORMAT_MOD_LINEAR {
                return Err(format!(
                    "Vulkan raw-buffer NV12 staging only supports linear modifier 0, got {:#018x}",
                    capability.modifier
                ));
            }
            if conversion.model != YcbcrModel::Bt709 {
                return Err("Vulkan staged NV12 conversion currently requires BT.709".to_string());
            }
            let required = vk::FormatFeatureFlags::SAMPLED_IMAGE
                | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR
                | vk::FormatFeatureFlags::STORAGE_IMAGE;
            if !capability.sampled_tiling_features.contains(required) {
                return Err(format!(
                    "Vulkan staged NV12 output lacks required optimal-image features 0x{:x} (available=0x{:x})",
                    required.as_raw(),
                    capability.sampled_tiling_features.as_raw()
                ));
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Nv12SourceCacheKey {
    stream_incarnation: u64,
    device: u64,
    inode: u64,
    topology: Nv12FrameTopology,
    strategy: Nv12ImportStrategy,
}

enum CachedNv12SourceAllocation<D: VulkanDeviceContext> {
    Compute {
        // The view must be destroyed before its buffer and imported memory.
        view: BufferViewAllocation<D>,
        source: BufferAllocation<D>,
    },
    Transfer {
        source: BufferAllocation<D>,
    },
}

impl<D: VulkanDeviceContext> CachedNv12SourceAllocation<D> {
    fn source(&self) -> &BufferAllocation<D> {
        match self {
            Self::Compute { source, .. } | Self::Transfer { source } => source,
        }
    }

    fn compute_view(&self) -> Option<vk::BufferView> {
        match self {
            Self::Compute { view, .. } => Some(view.view),
            Self::Transfer { .. } => None,
        }
    }
}

struct CachedNv12Source<D: VulkanDeviceContext> {
    allocation: CachedNv12SourceAllocation<D>,
    claimed: AtomicBool,
    last_used: AtomicU64,
}

fn claim_idle_source(claimed: &AtomicBool) -> Result<(), String> {
    claimed
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| {
            "cached NV12 DMA-BUF reappeared before its previous lease completed".to_string()
        })
}

impl<D: VulkanDeviceContext> CachedNv12Source<D> {
    fn claim(self: &Arc<Self>, generation: u64) -> Result<Nv12SourceLease<D>, String> {
        claim_idle_source(&self.claimed)?;
        self.last_used.store(generation, Ordering::Release);
        Ok(Nv12SourceLease {
            source: Arc::clone(self),
            released: AtomicBool::new(false),
        })
    }
}

struct Nv12SourceLease<D: VulkanDeviceContext> {
    source: Arc<CachedNv12Source<D>>,
    released: AtomicBool,
}

impl<D: VulkanDeviceContext> Nv12SourceLease<D> {
    fn release(&self) -> bool {
        if self.released.swap(true, Ordering::AcqRel) {
            return false;
        }
        let was_claimed = self.source.claimed.swap(false, Ordering::AcqRel);
        debug_assert!(was_claimed, "NV12 source cache claim released twice");
        true
    }

    fn is_released(&self) -> bool {
        self.released.load(Ordering::Acquire)
    }
}

impl<D: VulkanDeviceContext> Drop for Nv12SourceLease<D> {
    fn drop(&mut self) {
        self.release();
    }
}

struct Nv12SourceCache<D: VulkanDeviceContext> {
    entries: HashMap<Nv12SourceCacheKey, Arc<CachedNv12Source<D>>>,
    max_entries: usize,
}

impl<D: VulkanDeviceContext> Nv12SourceCache<D> {
    fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            max_entries,
        }
    }

    fn evict_one_idle(&mut self) -> bool {
        let idle = self
            .entries
            .iter()
            .filter(|(_, source)| {
                !source.claimed.load(Ordering::Acquire) && Arc::strong_count(source) == 1
            })
            .min_by_key(|(_, source)| source.last_used.load(Ordering::Acquire))
            .map(|(key, _)| *key);
        idle.is_some_and(|key| self.entries.remove(&key).is_some())
    }

    fn evict_stream(&mut self, stream_incarnation: u64) -> (usize, usize) {
        let before = self.entries.len();
        self.entries.retain(|key, source| {
            key.stream_incarnation != stream_incarnation
                || source.claimed.load(Ordering::Acquire)
                || Arc::strong_count(source) > 1
        });
        let evicted = before.saturating_sub(self.entries.len());
        let retained = self
            .entries
            .keys()
            .filter(|key| key.stream_incarnation == stream_incarnation)
            .count();
        (evicted, retained)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PackedSourceTopology {
    dimensions: (u32, u32),
    object_size: u64,
    modifier: u64,
    plane: ImportedPlane,
    format: PackedImageFormat,
    strategy: PackedImageImportStrategy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PackedSourceCacheKey {
    stream_incarnation: u64,
    device: u64,
    inode: u64,
    topology: PackedSourceTopology,
}

enum PackedSourceAllocation<D: VulkanDeviceContext> {
    Direct(ImageAllocation<D>),
    Staged {
        // The view must be destroyed before its buffer and imported memory.
        view: BufferViewAllocation<D>,
        source: BufferAllocation<D>,
    },
}

struct CachedPackedSource<D: VulkanDeviceContext> {
    allocation: PackedSourceAllocation<D>,
    format: PackedImageFormat,
    claimed: AtomicBool,
    last_used: AtomicU64,
}

impl<D: VulkanDeviceContext> CachedPackedSource<D> {
    fn claim(self: &Arc<Self>, generation: u64) -> Result<PackedSourceLease<D>, String> {
        self.claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                "cached packed DMA-BUF reappeared before its previous lease completed".to_string()
            })?;
        self.last_used.store(generation, Ordering::Release);
        Ok(PackedSourceLease {
            source: Arc::clone(self),
            released: AtomicBool::new(false),
        })
    }
}

struct PackedSourceLease<D: VulkanDeviceContext> {
    source: Arc<CachedPackedSource<D>>,
    released: AtomicBool,
}

impl<D: VulkanDeviceContext> PackedSourceLease<D> {
    fn release(&self) -> bool {
        if self.released.swap(true, Ordering::AcqRel) {
            return false;
        }
        let was_claimed = self.source.claimed.swap(false, Ordering::AcqRel);
        debug_assert!(was_claimed, "packed source cache claim released twice");
        true
    }

    fn is_released(&self) -> bool {
        self.released.load(Ordering::Acquire)
    }
}

impl<D: VulkanDeviceContext> Drop for PackedSourceLease<D> {
    fn drop(&mut self) {
        self.release();
    }
}

struct PackedSourceCache<D: VulkanDeviceContext> {
    entries: HashMap<PackedSourceCacheKey, Arc<CachedPackedSource<D>>>,
    max_entries: usize,
}

impl<D: VulkanDeviceContext> PackedSourceCache<D> {
    fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            max_entries,
        }
    }

    fn evict_one_idle(&mut self) -> bool {
        let idle = self
            .entries
            .iter()
            .filter(|(_, source)| {
                !source.claimed.load(Ordering::Acquire) && Arc::strong_count(source) == 1
            })
            .min_by_key(|(_, source)| source.last_used.load(Ordering::Acquire))
            .map(|(key, _)| *key);
        idle.is_some_and(|key| self.entries.remove(&key).is_some())
    }

    fn evict_stream(&mut self, stream_incarnation: u64) -> (usize, usize) {
        let before = self.entries.len();
        self.entries.retain(|key, source| {
            key.stream_incarnation != stream_incarnation
                || source.claimed.load(Ordering::Acquire)
                || Arc::strong_count(source) > 1
        });
        let evicted = before.saturating_sub(self.entries.len());
        let retained = self
            .entries
            .keys()
            .filter(|key| key.stream_incarnation == stream_incarnation)
            .count();
        (evicted, retained)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PackedImportCacheStats {
    pub source_entries: usize,
    pub active_sources: usize,
    pub output_slots: usize,
    pub active_outputs: usize,
    pub source_cache_hits: u64,
    pub source_cache_misses: u64,
    pub source_cache_evictions: u64,
    pub source_active_reuse_rejections: u64,
    pub source_topology_collisions: u64,
    pub allocation_size_rejections: u64,
    pub output_pool_busy_rejections: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Nv12ImportCacheStats {
    pub source_entries: usize,
    pub active_sources: usize,
    pub output_slots: usize,
    pub active_outputs: usize,
    pub source_cache_hits: u64,
    pub source_cache_misses: u64,
    pub source_cache_evictions: u64,
    pub source_active_reuse_rejections: u64,
    pub source_topology_collisions: u64,
    pub output_pool_busy_rejections: u64,
}

struct StagedNv12Import<D: VulkanDeviceContext> {
    stream_incarnation: u64,
    dimensions: (u32, u32),
    source_fd: i32,
    layout: Nv12SharedObjectLayout,
    conversion: Nv12Conversion,
    strategy: Nv12ImportStrategy,
    sampled_format_features: vk::FormatFeatureFlags,
    pipeline: Option<Arc<Nv12ComputePipeline<D>>>,
}

pub struct VulkanDmaBufImporter<D: VulkanDeviceContext> {
    device: Arc<D>,
    nv12_capabilities: Vec<Nv12ModifierCapability>,
    // Pools must drop before the descriptor-set layouts owned by their pipelines.
    nv12_output_pool: Mutex<Nv12OutputPool<D>>,
    nv12_source_cache: Mutex<Nv12SourceCache<D>>,
    packed_output_pool: Mutex<PackedOutputPool<D>>,
    packed_source_cache: Mutex<PackedSourceCache<D>>,
    nv12_rgba_compute: Option<Arc<Nv12ComputePipeline<D>>>,
    nv12_planar_compute: Option<Arc<Nv12ComputePipeline<D>>>,
    packed_bgra_compute: Option<Arc<PackedComputePipeline<D>>>,
    packed_bgra_staging_error: Option<String>,
    source_cache_hits: AtomicU64,
    source_cache_misses: AtomicU64,
    source_cache_evictions: AtomicU64,
    source_active_reuse_rejections: AtomicU64,
    source_topology_collisions: AtomicU64,
    output_pool_busy_rejections: AtomicU64,
    source_cache_clock: AtomicU64,
    packed_source_cache_hits: AtomicU64,
    packed_source_cache_misses: AtomicU64,
    packed_source_cache_evictions: AtomicU64,
    packed_source_active_reuse_rejections: AtomicU64,
    packed_source_topology_collisions: AtomicU64,
    packed_allocation_size_rejections: AtomicU64,
    packed_output_pool_busy_rejections: AtomicU64,
    packed_source_cache_clock: AtomicU64,
}

impl<D: VulkanDeviceContext> VulkanDmaBufImporter<D> {
    pub fn new(device: Arc<D>) -> Result<Self, String> {
        Self::new_with_limits(device, VulkanImportPoolLimits::default())
    }

    pub fn new_with_limits(device: Arc<D>, limits: VulkanImportPoolLimits) -> Result<Self, String> {
        Self::new_with_limits_and_staging_preference(
            device,
            limits,
            Nv12StagingPreference::default(),
        )
    }

    pub fn new_with_limits_and_staging_preference(
        device: Arc<D>,
        limits: VulkanImportPoolLimits,
        staging_preference: Nv12StagingPreference,
    ) -> Result<Self, String> {
        if limits.nv12_source_cache_entries == 0
            || limits.nv12_output_slots == 0
            || limits.packed_source_cache_entries == 0
            || limits.packed_output_slots == 0
        {
            return Err("Vulkan import pool limits must be non-zero".to_string());
        }
        let nv12_capabilities = inventory_nv12_modifier_capabilities_with_staging_preference(
            device.as_ref(),
            staging_preference,
        )?;
        let nv12_rgba_compute = if nv12_capabilities
            .iter()
            .any(|capability| capability.strategy == Nv12ImportStrategy::LinearBufferToRgba)
        {
            Some(Arc::new(Nv12ComputePipeline::new(
                Arc::clone(&device),
                Nv12ComputeOutput::Rgba,
            )?))
        } else {
            None
        };
        let nv12_planar_compute = if nv12_capabilities
            .iter()
            .any(|capability| capability.strategy == Nv12ImportStrategy::LinearBufferToYuvPlanes)
        {
            Some(Arc::new(Nv12ComputePipeline::new(
                Arc::clone(&device),
                Nv12ComputeOutput::YuvPlanes,
            )?))
        } else {
            None
        };
        let (packed_bgra_compute, packed_bgra_staging_error) = match validate_packed_staging_support(
            device.as_ref(),
            PackedImageFormat::Bgra8888,
            DRM_FORMAT_MOD_LINEAR,
        ) {
            Ok(()) => (
                Some(Arc::new(PackedComputePipeline::new(Arc::clone(&device))?)),
                None,
            ),
            Err(error) => (None, Some(error)),
        };
        Ok(Self {
            device,
            nv12_capabilities,
            nv12_output_pool: Mutex::new(Nv12OutputPool::new(limits.nv12_output_slots)),
            nv12_source_cache: Mutex::new(Nv12SourceCache::new(limits.nv12_source_cache_entries)),
            packed_output_pool: Mutex::new(PackedOutputPool::new(limits.packed_output_slots)),
            packed_source_cache: Mutex::new(PackedSourceCache::new(
                limits.packed_source_cache_entries,
            )),
            nv12_rgba_compute,
            nv12_planar_compute,
            packed_bgra_compute,
            packed_bgra_staging_error,
            source_cache_hits: AtomicU64::new(0),
            source_cache_misses: AtomicU64::new(0),
            source_cache_evictions: AtomicU64::new(0),
            source_active_reuse_rejections: AtomicU64::new(0),
            source_topology_collisions: AtomicU64::new(0),
            output_pool_busy_rejections: AtomicU64::new(0),
            source_cache_clock: AtomicU64::new(1),
            packed_source_cache_hits: AtomicU64::new(0),
            packed_source_cache_misses: AtomicU64::new(0),
            packed_source_cache_evictions: AtomicU64::new(0),
            packed_source_active_reuse_rejections: AtomicU64::new(0),
            packed_source_topology_collisions: AtomicU64::new(0),
            packed_allocation_size_rejections: AtomicU64::new(0),
            packed_output_pool_busy_rejections: AtomicU64::new(0),
            packed_source_cache_clock: AtomicU64::new(1),
        })
    }

    pub fn device(&self) -> &Arc<D> {
        &self.device
    }

    pub fn nv12_capabilities(&self) -> &[Nv12ModifierCapability] {
        &self.nv12_capabilities
    }

    pub fn packed_import_strategy(
        &self,
        format: PackedImageFormat,
        modifier: u64,
    ) -> Result<PackedImageImportStrategy, String> {
        match validate_packed_import_support(self.device.as_ref(), format, modifier) {
            Ok(()) => Ok(PackedImageImportStrategy::DirectSampledImage),
            Err(direct_error) => {
                validate_packed_staging_support(self.device.as_ref(), format, modifier).map_err(
                    |staged_error| {
                        format!(
                            "Vulkan packed import has neither direct nor staged support: direct={direct_error}; staged={staged_error}"
                        )
                    },
                )?;
                if self.packed_bgra_compute.is_none() {
                    return Err(format!(
                        "Vulkan packed staging pipeline is unavailable: {}",
                        self.packed_bgra_staging_error
                            .as_deref()
                            .unwrap_or("pipeline initialization failed without a diagnostic")
                    ));
                }
                Ok(PackedImageImportStrategy::LinearBufferToOptimalBgra)
            }
        }
    }

    /// Imports a packed image using the historical direct-sampled contract.
    pub fn import_packed(
        &self,
        request: PackedImageImport,
    ) -> Result<ImportedDmaBufImage<D>, String> {
        self.import_packed_with_strategy(request, PackedImageImportStrategy::DirectSampledImage)
    }

    pub fn import_packed_with_strategy(
        &self,
        request: PackedImageImport,
        strategy: PackedImageImportStrategy,
    ) -> Result<ImportedDmaBufImage<D>, String> {
        let source = self.claim_cached_packed_source(request, strategy)?;
        match strategy {
            PackedImageImportStrategy::DirectSampledImage => {
                ImportedDmaBufImage::from_direct_packed_source(
                    request.dimensions,
                    request.modifier,
                    source,
                )
            }
            PackedImageImportStrategy::LinearBufferToOptimalBgra => {
                let source_view = match &source.source.allocation {
                    PackedSourceAllocation::Staged { view, .. } => view.view,
                    PackedSourceAllocation::Direct(_) => {
                        return Err(
                            "Vulkan staged packed import received a direct-image cache entry"
                                .to_string(),
                        );
                    }
                };
                let pipeline = self.packed_bgra_compute.as_ref().cloned().ok_or_else(|| {
                    "Vulkan packed BGRA staging pipeline is unavailable".to_string()
                })?;
                let output = {
                    let mut pool = self
                        .packed_output_pool
                        .lock()
                        .map_err(|_| "Vulkan packed output-pool lock poisoned".to_string())?;
                    match pool.claim(
                        Arc::clone(&self.device),
                        request.dimensions,
                        &pipeline,
                        source_view,
                    ) {
                        Ok(output) => output,
                        Err(error) => {
                            if error.starts_with("Vulkan packed output pool is saturated") {
                                self.packed_output_pool_busy_rejections
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                            return Err(error);
                        }
                    }
                };
                create_staged_packed(request, source, output, pipeline)
            }
        }
    }

    pub fn evict_packed_stream(&self, stream_incarnation: u64) -> Result<(), String> {
        let (evicted, retained) = self
            .packed_source_cache
            .lock()
            .map_err(|_| "Vulkan packed source-cache lock poisoned".to_string())?
            .evict_stream(stream_incarnation);
        self.packed_source_cache_evictions
            .fetch_add(evicted as u64, Ordering::Relaxed);
        if retained == 0 {
            Ok(())
        } else {
            Err(format!(
                "Vulkan packed stream {stream_incarnation} still owns {retained} active cached source(s)"
            ))
        }
    }

    pub fn packed_cache_stats(&self) -> Result<PackedImportCacheStats, String> {
        let cache = self
            .packed_source_cache
            .lock()
            .map_err(|_| "Vulkan packed source-cache lock poisoned".to_string())?;
        let output_pool = self
            .packed_output_pool
            .lock()
            .map_err(|_| "Vulkan packed output-pool lock poisoned".to_string())?;
        Ok(PackedImportCacheStats {
            source_entries: cache.entries.len(),
            active_sources: cache
                .entries
                .values()
                .filter(|source| source.claimed.load(Ordering::Acquire))
                .count(),
            output_slots: output_pool.slots.len(),
            active_outputs: output_pool
                .slots
                .iter()
                .filter(|slot| slot.claimed.load(Ordering::Acquire))
                .count(),
            source_cache_hits: self.packed_source_cache_hits.load(Ordering::Relaxed),
            source_cache_misses: self.packed_source_cache_misses.load(Ordering::Relaxed),
            source_cache_evictions: self.packed_source_cache_evictions.load(Ordering::Relaxed),
            source_active_reuse_rejections: self
                .packed_source_active_reuse_rejections
                .load(Ordering::Relaxed),
            source_topology_collisions: self
                .packed_source_topology_collisions
                .load(Ordering::Relaxed),
            allocation_size_rejections: self
                .packed_allocation_size_rejections
                .load(Ordering::Relaxed),
            output_pool_busy_rejections: self
                .packed_output_pool_busy_rejections
                .load(Ordering::Relaxed),
        })
    }

    fn claim_cached_packed_source(
        &self,
        request: PackedImageImport,
        strategy: PackedImageImportStrategy,
    ) -> Result<PackedSourceLease<D>, String> {
        let PackedImageImport {
            stream_incarnation,
            dimensions,
            source_fd,
            source_size,
            modifier,
            plane,
            format,
        } = request;
        validate_image_inputs(dimensions, source_fd, plane)?;
        if let Err(error) = validate_packed_layout(dimensions, source_size, plane) {
            if error.starts_with("DMA-BUF object size ") {
                self.packed_allocation_size_rejections
                    .fetch_add(1, Ordering::Relaxed);
            }
            return Err(error);
        }
        match strategy {
            PackedImageImportStrategy::DirectSampledImage => {
                validate_packed_import_support(self.device.as_ref(), format, modifier)?;
            }
            PackedImageImportStrategy::LinearBufferToOptimalBgra => {
                validate_packed_staging_support(self.device.as_ref(), format, modifier)?;
                if let Err(error) = validate_staged_packed_layout(self.device.as_ref(), source_size)
                {
                    self.packed_allocation_size_rejections
                        .fetch_add(1, Ordering::Relaxed);
                    return Err(error);
                }
            }
        }
        let (device, inode) = dmabuf_identity(source_fd)?;
        let topology = PackedSourceTopology {
            dimensions,
            object_size: source_size,
            modifier,
            plane,
            format,
            strategy,
        };
        let key = PackedSourceCacheKey {
            stream_incarnation,
            device,
            inode,
            topology,
        };
        let generation = self
            .packed_source_cache_clock
            .fetch_add(1, Ordering::Relaxed);
        {
            let cache = self
                .packed_source_cache
                .lock()
                .map_err(|_| "Vulkan packed source-cache lock poisoned".to_string())?;
            if let Some(source) = cache.entries.get(&key) {
                return match source.claim(generation) {
                    Ok(lease) => {
                        self.packed_source_cache_hits
                            .fetch_add(1, Ordering::Relaxed);
                        Ok(lease)
                    }
                    Err(error) => {
                        self.packed_source_active_reuse_rejections
                            .fetch_add(1, Ordering::Relaxed);
                        Err(error)
                    }
                };
            }
            if cache.entries.keys().any(|existing| {
                existing.stream_incarnation == stream_incarnation
                    && existing.device == device
                    && existing.inode == inode
                    && existing.topology != topology
            }) {
                self.packed_source_topology_collisions
                    .fetch_add(1, Ordering::Relaxed);
                return Err(
                    "cached packed DMA-BUF identity reappeared with different topology".to_string(),
                );
            }
        }

        self.packed_source_cache_misses
            .fetch_add(1, Ordering::Relaxed);
        let allocation = match strategy {
            PackedImageImportStrategy::DirectSampledImage => create_direct_image(
                Arc::clone(&self.device),
                dimensions,
                source_fd,
                modifier,
                &[plane],
                format.vk_format(),
                DIRECT_PACKED_USAGE,
                Some(source_size),
            )
            .map(PackedSourceAllocation::Direct),
            PackedImageImportStrategy::LinearBufferToOptimalBgra => {
                create_cached_packed_staged_source(Arc::clone(&self.device), source_fd, source_size)
            }
        };
        let allocation = match allocation {
            Ok(allocation) => allocation,
            Err(error) => {
                if error.starts_with("DMA-BUF object size ")
                    || error.starts_with("DMA-BUF allocation size ")
                {
                    self.packed_allocation_size_rejections
                        .fetch_add(1, Ordering::Relaxed);
                }
                return Err(error);
            }
        };
        let source = Arc::new(CachedPackedSource {
            allocation,
            format,
            claimed: AtomicBool::new(false),
            last_used: AtomicU64::new(generation),
        });
        let mut cache = self
            .packed_source_cache
            .lock()
            .map_err(|_| "Vulkan packed source-cache lock poisoned".to_string())?;
        if let Some(existing) = cache.entries.get(&key) {
            return match existing.claim(generation) {
                Ok(lease) => {
                    self.packed_source_cache_hits
                        .fetch_add(1, Ordering::Relaxed);
                    Ok(lease)
                }
                Err(error) => {
                    self.packed_source_active_reuse_rejections
                        .fetch_add(1, Ordering::Relaxed);
                    Err(error)
                }
            };
        }
        if cache.entries.keys().any(|existing| {
            existing.stream_incarnation == stream_incarnation
                && existing.device == device
                && existing.inode == inode
                && existing.topology != topology
        }) {
            self.packed_source_topology_collisions
                .fetch_add(1, Ordering::Relaxed);
            return Err(
                "cached packed DMA-BUF identity reappeared with different topology".to_string(),
            );
        }
        if cache.entries.len() >= cache.max_entries && cache.evict_one_idle() {
            self.packed_source_cache_evictions
                .fetch_add(1, Ordering::Relaxed);
        }
        if cache.entries.len() >= cache.max_entries {
            return Err(format!(
                "Vulkan packed source cache is saturated at {} entries",
                cache.max_entries
            ));
        }
        let lease = source.claim(generation)?;
        cache.entries.insert(key, source);
        Ok(lease)
    }

    pub fn import_nv12_shared_object(
        &self,
        stream_incarnation: u64,
        dimensions: (u32, u32),
        source_fd: i32,
        layout: Nv12SharedObjectLayout,
        conversion: Nv12Conversion,
        capability: Nv12ModifierCapability,
    ) -> Result<ImportedDmaBufImage<D>, String> {
        if !self
            .nv12_capabilities
            .iter()
            .copied()
            .any(|admitted| same_nv12_capability(admitted, capability))
        {
            return Err(format!(
                "Vulkan NV12 modifier {:#018x} capability was not issued by this active-device importer",
                capability.modifier
            ));
        }
        if capability.modifier != layout.modifier {
            return Err("Vulkan NV12 capability modifier does not match frame layout".to_string());
        }
        validate_nv12_modifier_capability(capability, dimensions, conversion)?;
        validate_nv12_shared_layout(dimensions, layout)?;
        match capability.strategy {
            Nv12ImportStrategy::DirectSampledImage => {
                ImportedDmaBufImage::new_nv12_direct_shared_object(
                    Arc::clone(&self.device),
                    dimensions,
                    source_fd,
                    layout,
                    conversion,
                    capability,
                )
            }
            Nv12ImportStrategy::LinearBufferToOptimalNv12
            | Nv12ImportStrategy::LinearBufferToOptimalYuvPlanes => {
                self.create_staged_nv12(StagedNv12Import {
                    stream_incarnation,
                    dimensions,
                    source_fd,
                    layout,
                    conversion,
                    strategy: capability.strategy,
                    sampled_format_features: capability.sampled_tiling_features,
                    pipeline: None,
                })
            }
            Nv12ImportStrategy::LinearBufferToYuvPlanes => {
                let pipeline = self.nv12_planar_compute.as_ref().cloned().ok_or_else(|| {
                    "Vulkan NV12 planar staging pipeline is unavailable".to_string()
                })?;
                self.create_staged_nv12(StagedNv12Import {
                    stream_incarnation,
                    dimensions,
                    source_fd,
                    layout,
                    conversion,
                    strategy: capability.strategy,
                    sampled_format_features: capability.sampled_tiling_features,
                    pipeline: Some(pipeline),
                })
            }
            Nv12ImportStrategy::LinearBufferToRgba => {
                let pipeline = self.nv12_rgba_compute.as_ref().cloned().ok_or_else(|| {
                    "Vulkan NV12 RGBA staging pipeline is unavailable".to_string()
                })?;
                self.create_staged_nv12(StagedNv12Import {
                    stream_incarnation,
                    dimensions,
                    source_fd,
                    layout,
                    conversion,
                    strategy: capability.strategy,
                    sampled_format_features: capability.sampled_tiling_features,
                    pipeline: Some(pipeline),
                })
            }
        }
    }

    pub fn evict_nv12_stream(&self, stream_incarnation: u64) -> Result<(), String> {
        let (evicted, retained) = self
            .nv12_source_cache
            .lock()
            .map_err(|_| "Vulkan NV12 source-cache lock poisoned".to_string())?
            .evict_stream(stream_incarnation);
        self.source_cache_evictions
            .fetch_add(evicted as u64, Ordering::Relaxed);
        if retained == 0 {
            Ok(())
        } else {
            Err(format!(
                "Vulkan NV12 stream {stream_incarnation} still owns {retained} active cached source(s)"
            ))
        }
    }

    pub fn nv12_cache_stats(&self) -> Result<Nv12ImportCacheStats, String> {
        let source_cache = self
            .nv12_source_cache
            .lock()
            .map_err(|_| "Vulkan NV12 source-cache lock poisoned".to_string())?;
        let output_pool = self
            .nv12_output_pool
            .lock()
            .map_err(|_| "Vulkan NV12 output-pool lock poisoned".to_string())?;
        Ok(Nv12ImportCacheStats {
            source_entries: source_cache.entries.len(),
            active_sources: source_cache
                .entries
                .values()
                .filter(|source| source.claimed.load(Ordering::Acquire))
                .count(),
            output_slots: output_pool.slots.len(),
            active_outputs: output_pool
                .slots
                .iter()
                .filter(|slot| slot.claimed.load(Ordering::Acquire))
                .count(),
            source_cache_hits: self.source_cache_hits.load(Ordering::Relaxed),
            source_cache_misses: self.source_cache_misses.load(Ordering::Relaxed),
            source_cache_evictions: self.source_cache_evictions.load(Ordering::Relaxed),
            source_active_reuse_rejections: self
                .source_active_reuse_rejections
                .load(Ordering::Relaxed),
            source_topology_collisions: self.source_topology_collisions.load(Ordering::Relaxed),
            output_pool_busy_rejections: self.output_pool_busy_rejections.load(Ordering::Relaxed),
        })
    }

    fn claim_cached_source(
        &self,
        stream_incarnation: u64,
        source_fd: i32,
        topology: Nv12FrameTopology,
        strategy: Nv12ImportStrategy,
    ) -> Result<Nv12SourceLease<D>, String> {
        let (device_id, inode) = dmabuf_identity(source_fd)?;
        let generation = self.source_cache_clock.fetch_add(1, Ordering::Relaxed);
        let key = Nv12SourceCacheKey {
            stream_incarnation,
            device: device_id,
            inode,
            topology,
            strategy,
        };
        {
            let cache = self
                .nv12_source_cache
                .lock()
                .map_err(|_| "Vulkan NV12 source-cache lock poisoned".to_string())?;
            if let Some(source) = cache.entries.get(&key) {
                return match source.claim(generation) {
                    Ok(lease) => {
                        self.source_cache_hits.fetch_add(1, Ordering::Relaxed);
                        Ok(lease)
                    }
                    Err(error) => {
                        self.source_active_reuse_rejections
                            .fetch_add(1, Ordering::Relaxed);
                        Err(error)
                    }
                };
            }
            if cache.entries.keys().any(|existing| {
                existing.stream_incarnation == stream_incarnation
                    && existing.device == device_id
                    && existing.inode == inode
                    && (existing.topology != topology || existing.strategy != strategy)
            }) {
                self.source_topology_collisions
                    .fetch_add(1, Ordering::Relaxed);
                return Err(
                    "cached NV12 DMA-BUF identity reappeared with different topology or read strategy"
                        .to_string(),
                );
            }
        }

        self.source_cache_misses.fetch_add(1, Ordering::Relaxed);
        let source = Arc::new(create_cached_nv12_source(
            Arc::clone(&self.device),
            source_fd,
            topology.object_size,
            strategy,
        )?);
        let mut cache = self
            .nv12_source_cache
            .lock()
            .map_err(|_| "Vulkan NV12 source-cache lock poisoned".to_string())?;
        if let Some(existing) = cache.entries.get(&key) {
            return match existing.claim(generation) {
                Ok(lease) => {
                    self.source_cache_hits.fetch_add(1, Ordering::Relaxed);
                    Ok(lease)
                }
                Err(error) => {
                    self.source_active_reuse_rejections
                        .fetch_add(1, Ordering::Relaxed);
                    Err(error)
                }
            };
        }
        if cache.entries.keys().any(|existing| {
            existing.stream_incarnation == stream_incarnation
                && existing.device == device_id
                && existing.inode == inode
                && (existing.topology != topology || existing.strategy != strategy)
        }) {
            self.source_topology_collisions
                .fetch_add(1, Ordering::Relaxed);
            return Err(
                "cached NV12 DMA-BUF identity reappeared with different topology or read strategy"
                    .to_string(),
            );
        }
        if cache.entries.len() >= cache.max_entries && cache.evict_one_idle() {
            self.source_cache_evictions.fetch_add(1, Ordering::Relaxed);
        }
        if cache.entries.len() >= cache.max_entries {
            return Err(format!(
                "Vulkan NV12 source cache is saturated at {} entries",
                cache.max_entries
            ));
        }
        let lease = source.claim(generation)?;
        cache.entries.insert(key, source);
        Ok(lease)
    }

    fn create_staged_nv12(
        &self,
        request: StagedNv12Import<D>,
    ) -> Result<ImportedDmaBufImage<D>, String> {
        let StagedNv12Import {
            stream_incarnation,
            dimensions,
            source_fd,
            layout,
            conversion,
            strategy,
            sampled_format_features,
            pipeline,
        } = request;
        match strategy {
            Nv12ImportStrategy::LinearBufferToOptimalNv12
            | Nv12ImportStrategy::LinearBufferToOptimalYuvPlanes => {
                validate_transfer_layout(dimensions, layout)?;
            }
            Nv12ImportStrategy::LinearBufferToYuvPlanes
            | Nv12ImportStrategy::LinearBufferToRgba => {
                validate_staged_layout(self.device.as_ref(), layout)?;
            }
            Nv12ImportStrategy::DirectSampledImage => {
                return Err("direct NV12 import cannot use staged construction".to_string());
            }
        }
        let topology = layout.frame_topology(dimensions);
        let source = self.claim_cached_source(stream_incarnation, source_fd, topology, strategy)?;
        let source_view = source.source.allocation.compute_view();
        let output = {
            let mut pool = self
                .nv12_output_pool
                .lock()
                .map_err(|_| "Vulkan NV12 output-pool lock poisoned".to_string())?;
            match pool.claim(
                Arc::clone(&self.device),
                dimensions,
                strategy,
                pipeline.as_deref(),
                source_view,
            ) {
                Ok(output) => output,
                Err(error) => {
                    if error.starts_with("Vulkan NV12 output pool is saturated") {
                        self.output_pool_busy_rejections
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    return Err(error);
                }
            }
        };
        create_staged_nv12(
            dimensions,
            layout,
            conversion,
            source,
            output,
            pipeline,
            strategy,
            sampled_format_features,
        )
    }
}

fn same_nv12_capability(left: Nv12ModifierCapability, right: Nv12ModifierCapability) -> bool {
    left.modifier == right.modifier
        && left.strategy == right.strategy
        && left.modifier_plane_count == right.modifier_plane_count
        && left.source_tiling_features == right.source_tiling_features
        && left.sampled_tiling_features == right.sampled_tiling_features
        && left.external_features == right.external_features
        && left.compatible_handle_types == right.compatible_handle_types
        && left.max_extent.width == right.max_extent.width
        && left.max_extent.height == right.max_extent.height
        && left.max_extent.depth == right.max_extent.depth
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampledImageFormat {
    Rgba8888,
    Bgra8888,
    Nv12,
    Nv12Planes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StagedNv12Planes {
    pub luma_image: vk::Image,
    pub chroma_image: vk::Image,
    pub conversion: Nv12Conversion,
}

#[derive(Clone, Copy)]
enum StagedSampledImages {
    Rgba { image: vk::Image },
    Bgra { image: vk::Image },
    Nv12 { image: vk::Image },
    YuvPlanes { luma: vk::Image, chroma: vk::Image },
}

#[derive(Clone, Copy)]
pub struct DirectNv12Sampling {
    pub conversion: Nv12Conversion,
    pub format_features: vk::FormatFeatureFlags,
}

struct ImageAllocation<D: VulkanDeviceContext> {
    device: Arc<D>,
    image: vk::Image,
    memory: vk::DeviceMemory,
}

impl<D: VulkanDeviceContext> Drop for ImageAllocation<D> {
    fn drop(&mut self) {
        unsafe {
            self.device.device().destroy_image(self.image, None);
            self.device.device().free_memory(self.memory, None);
        }
    }
}

struct BufferAllocation<D: VulkanDeviceContext> {
    device: Arc<D>,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    size: u64,
}

impl<D: VulkanDeviceContext> Drop for BufferAllocation<D> {
    fn drop(&mut self) {
        unsafe {
            self.device.device().destroy_buffer(self.buffer, None);
            self.device.device().free_memory(self.memory, None);
        }
    }
}

struct BufferViewAllocation<D: VulkanDeviceContext> {
    device: Arc<D>,
    view: vk::BufferView,
}

impl<D: VulkanDeviceContext> Drop for BufferViewAllocation<D> {
    fn drop(&mut self) {
        unsafe { self.device.device().destroy_buffer_view(self.view, None) };
    }
}

struct ImageViewAllocation<D: VulkanDeviceContext> {
    device: Arc<D>,
    view: vk::ImageView,
}

impl<D: VulkanDeviceContext> Drop for ImageViewAllocation<D> {
    fn drop(&mut self) {
        unsafe { self.device.device().destroy_image_view(self.view, None) };
    }
}

enum StagedOutputResources<D: VulkanDeviceContext> {
    Rgba {
        view: ImageViewAllocation<D>,
        sampled: ImageAllocation<D>,
    },
    Nv12 {
        sampled: ImageAllocation<D>,
    },
    TransferYuvPlanes {
        luma: ImageAllocation<D>,
        chroma: ImageAllocation<D>,
    },
    YuvPlanes {
        luma_view: ImageViewAllocation<D>,
        luma: ImageAllocation<D>,
        chroma_view: ImageViewAllocation<D>,
        chroma: ImageAllocation<D>,
    },
}

struct StagedOutputSlot<D: VulkanDeviceContext> {
    device: Arc<D>,
    dimensions: (u32, u32),
    strategy: Nv12ImportStrategy,
    descriptor_pool: Option<vk::DescriptorPool>,
    descriptor_set: Option<vk::DescriptorSet>,
    resources: StagedOutputResources<D>,
    claimed: AtomicBool,
    initialized: AtomicBool,
}

impl<D: VulkanDeviceContext> StagedOutputSlot<D> {
    fn sampled_images(&self) -> StagedSampledImages {
        match &self.resources {
            StagedOutputResources::Rgba { sampled, .. } => StagedSampledImages::Rgba {
                image: sampled.image,
            },
            StagedOutputResources::Nv12 { sampled } => StagedSampledImages::Nv12 {
                image: sampled.image,
            },
            StagedOutputResources::TransferYuvPlanes { luma, chroma }
            | StagedOutputResources::YuvPlanes { luma, chroma, .. } => {
                StagedSampledImages::YuvPlanes {
                    luma: luma.image,
                    chroma: chroma.image,
                }
            }
        }
    }
}

impl<D: VulkanDeviceContext> Drop for StagedOutputSlot<D> {
    fn drop(&mut self) {
        if let Some(descriptor_pool) = self.descriptor_pool {
            unsafe {
                self.device
                    .device()
                    .destroy_descriptor_pool(descriptor_pool, None);
            }
        }
    }
}

struct Nv12OutputPool<D: VulkanDeviceContext> {
    slots: Vec<Arc<StagedOutputSlot<D>>>,
    max_slots: usize,
}

impl<D: VulkanDeviceContext> Nv12OutputPool<D> {
    fn new(max_slots: usize) -> Self {
        Self {
            slots: Vec::new(),
            max_slots,
        }
    }

    fn claim(
        &mut self,
        device: Arc<D>,
        dimensions: (u32, u32),
        strategy: Nv12ImportStrategy,
        pipeline: Option<&Nv12ComputePipeline<D>>,
        source_view: Option<vk::BufferView>,
    ) -> Result<Nv12OutputLease<D>, String> {
        if let Some(slot) = self.slots.iter().find(|slot| {
            slot.dimensions == dimensions
                && slot.strategy == strategy
                && slot
                    .claimed
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
        }) {
            let lease = Nv12OutputLease {
                slot: Arc::clone(slot),
                released: AtomicBool::new(false),
            };
            if let Some(source_view) = source_view {
                update_staged_descriptor_set(device.as_ref(), slot, source_view)?;
            }
            return Ok(lease);
        }
        if self.slots.len() >= self.max_slots {
            if let Some(idle) = self.slots.iter().position(|slot| {
                !slot.claimed.load(Ordering::Acquire)
                    && (slot.dimensions != dimensions || slot.strategy != strategy)
            }) {
                self.slots.swap_remove(idle);
            } else {
                return Err(format!(
                    "Vulkan NV12 output pool is saturated at {} slots",
                    self.max_slots
                ));
            }
        }
        let slot = Arc::new(create_staged_output_slot(
            device,
            dimensions,
            strategy,
            pipeline,
            source_view,
        )?);
        slot.claimed.store(true, Ordering::Release);
        self.slots.push(Arc::clone(&slot));
        Ok(Nv12OutputLease {
            slot,
            released: AtomicBool::new(false),
        })
    }
}

struct Nv12OutputLease<D: VulkanDeviceContext> {
    slot: Arc<StagedOutputSlot<D>>,
    released: AtomicBool,
}

impl<D: VulkanDeviceContext> Nv12OutputLease<D> {
    fn release(&self) {
        if !self.released.swap(true, Ordering::AcqRel) {
            let was_claimed = self.slot.claimed.swap(false, Ordering::AcqRel);
            debug_assert!(was_claimed, "NV12 output slot released twice");
        }
    }
}

impl<D: VulkanDeviceContext> Drop for Nv12OutputLease<D> {
    fn drop(&mut self) {
        self.release();
    }
}

struct PackedOutputSlot<D: VulkanDeviceContext> {
    device: Arc<D>,
    dimensions: (u32, u32),
    descriptor_pool: vk::DescriptorPool,
    descriptor_set: vk::DescriptorSet,
    // The storage view must be destroyed before the image it aliases.
    storage_view: ImageViewAllocation<D>,
    sampled: ImageAllocation<D>,
    claimed: AtomicBool,
    initialized: AtomicBool,
}

impl<D: VulkanDeviceContext> Drop for PackedOutputSlot<D> {
    fn drop(&mut self) {
        unsafe {
            self.device
                .device()
                .destroy_descriptor_pool(self.descriptor_pool, None);
        }
    }
}

struct PackedOutputPool<D: VulkanDeviceContext> {
    slots: Vec<Arc<PackedOutputSlot<D>>>,
    max_slots: usize,
}

impl<D: VulkanDeviceContext> PackedOutputPool<D> {
    fn new(max_slots: usize) -> Self {
        Self {
            slots: Vec::new(),
            max_slots,
        }
    }

    fn claim(
        &mut self,
        device: Arc<D>,
        dimensions: (u32, u32),
        pipeline: &PackedComputePipeline<D>,
        source_view: vk::BufferView,
    ) -> Result<PackedOutputLease<D>, String> {
        if let Some(slot) = self.slots.iter().find(|slot| {
            slot.dimensions == dimensions
                && slot
                    .claimed
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
        }) {
            update_packed_descriptor_set(device.as_ref(), slot, source_view);
            return Ok(PackedOutputLease {
                slot: Arc::clone(slot),
                released: AtomicBool::new(false),
            });
        }
        if self.slots.len() >= self.max_slots {
            if let Some(idle) = self.slots.iter().position(|slot| {
                !slot.claimed.load(Ordering::Acquire) && slot.dimensions != dimensions
            }) {
                self.slots.swap_remove(idle);
            } else {
                return Err(format!(
                    "Vulkan packed output pool is saturated at {} slots",
                    self.max_slots
                ));
            }
        }
        let slot = Arc::new(create_packed_output_slot(
            device,
            dimensions,
            pipeline,
            source_view,
        )?);
        slot.claimed.store(true, Ordering::Release);
        self.slots.push(Arc::clone(&slot));
        Ok(PackedOutputLease {
            slot,
            released: AtomicBool::new(false),
        })
    }
}

struct PackedOutputLease<D: VulkanDeviceContext> {
    slot: Arc<PackedOutputSlot<D>>,
    released: AtomicBool,
}

impl<D: VulkanDeviceContext> PackedOutputLease<D> {
    fn release(&self) {
        if !self.released.swap(true, Ordering::AcqRel) {
            let was_claimed = self.slot.claimed.swap(false, Ordering::AcqRel);
            debug_assert!(was_claimed, "packed output slot released twice");
        }
    }
}

impl<D: VulkanDeviceContext> Drop for PackedOutputLease<D> {
    fn drop(&mut self) {
        self.release();
    }
}

struct StagedPacked<D: VulkanDeviceContext> {
    source: PackedSourceLease<D>,
    output: PackedOutputLease<D>,
    pipeline: Arc<PackedComputePipeline<D>>,
    push_constants: PackedPushConstants,
}

enum StagedNv12Operation<D: VulkanDeviceContext> {
    Compute {
        pipeline: Arc<Nv12ComputePipeline<D>>,
        push_constants: Nv12PushConstants,
    },
    Transfer {
        layout: Nv12SharedObjectLayout,
    },
}

struct StagedNv12<D: VulkanDeviceContext> {
    source: Nv12SourceLease<D>,
    output: Nv12OutputLease<D>,
    operation: StagedNv12Operation<D>,
    conversion: Nv12Conversion,
    format_features: vk::FormatFeatureFlags,
}

enum ImportedKind<D: VulkanDeviceContext> {
    DirectPacked(PackedSourceLease<D>),
    DirectBgraScanout(ImageAllocation<D>),
    DirectNv12(ImageAllocation<D>, DirectNv12Sampling),
    StagedPacked(StagedPacked<D>),
    StagedNv12(StagedNv12<D>),
}

/// One renderer-ready import. Direct imports alias producer memory; staged imports retain a
/// persistent cached source claim and one bounded renderer-native output slot.
pub struct ImportedDmaBufImage<D: VulkanDeviceContext> {
    kind: ImportedKind<D>,
    dimensions: (u32, u32),
    modifier: u64,
    sampled_usage: vk::ImageUsageFlags,
    sampled_tiling: vk::ImageTiling,
}

impl<D: VulkanDeviceContext> ImportedDmaBufImage<D> {
    pub fn new_bgra_scanout(
        device: Arc<D>,
        dimensions: (u32, u32),
        source_fd: i32,
        source_size: u64,
        modifier: u64,
        plane: ImportedPlane,
        usage: vk::ImageUsageFlags,
    ) -> Result<Self, String> {
        validate_image_inputs(dimensions, source_fd, plane)?;
        if source_size == 0 {
            return Err("Vulkan scanout import has a zero allocation size".to_string());
        }
        validate_bgra_scanout_import_support(device.as_ref(), modifier, usage)?;
        let sampled = create_direct_image(
            Arc::clone(&device),
            dimensions,
            source_fd,
            modifier,
            &[plane],
            IMPORTED_SCANOUT_BGRA_FORMAT,
            usage,
            Some(source_size),
        )?;
        Ok(Self {
            kind: ImportedKind::DirectBgraScanout(sampled),
            dimensions,
            modifier,
            sampled_usage: usage,
            sampled_tiling: vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
        })
    }

    fn from_direct_packed_source(
        dimensions: (u32, u32),
        modifier: u64,
        source: PackedSourceLease<D>,
    ) -> Result<Self, String> {
        if !matches!(&source.source.allocation, PackedSourceAllocation::Direct(_)) {
            return Err(
                "Vulkan direct packed import received a staged-buffer cache entry".to_string(),
            );
        }
        Ok(Self {
            kind: ImportedKind::DirectPacked(source),
            dimensions,
            modifier,
            sampled_usage: DIRECT_PACKED_USAGE,
            sampled_tiling: vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn new_nv12_direct_shared_object(
        device: Arc<D>,
        dimensions: (u32, u32),
        source_fd: i32,
        layout: Nv12SharedObjectLayout,
        conversion: Nv12Conversion,
        capability: Nv12ModifierCapability,
    ) -> Result<Self, String> {
        if source_fd < 0 {
            return Err("Vulkan NV12 import has an invalid DMA-BUF fd".to_string());
        }
        if capability.modifier != layout.modifier {
            return Err("Vulkan NV12 capability modifier does not match frame layout".to_string());
        }
        validate_nv12_modifier_capability(capability, dimensions, conversion)?;
        if capability.strategy != Nv12ImportStrategy::DirectSampledImage {
            return Err("Vulkan NV12 direct constructor received a staged capability".to_string());
        }
        let sampled = create_direct_image(
            Arc::clone(&device),
            dimensions,
            source_fd,
            layout.modifier,
            &layout.planes,
            IMPORTED_NV12_FORMAT,
            DIRECT_NV12_USAGE,
            Some(layout.object_size),
        )?;
        Ok(Self {
            kind: ImportedKind::DirectNv12(
                sampled,
                DirectNv12Sampling {
                    conversion,
                    format_features: capability.sampled_tiling_features,
                },
            ),
            dimensions,
            modifier: layout.modifier,
            sampled_usage: DIRECT_NV12_USAGE,
            sampled_tiling: vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT,
        })
    }

    pub fn image(&self) -> vk::Image {
        match &self.kind {
            ImportedKind::DirectPacked(source) => match &source.source.allocation {
                PackedSourceAllocation::Direct(allocation) => allocation.image,
                PackedSourceAllocation::Staged { .. } => {
                    unreachable!("direct packed import owns a direct image")
                }
            },
            ImportedKind::DirectBgraScanout(sampled) | ImportedKind::DirectNv12(sampled, _) => {
                sampled.image
            }
            ImportedKind::StagedPacked(staged) => staged.output.slot.sampled.image,
            ImportedKind::StagedNv12(staged) => match staged.output.slot.sampled_images() {
                StagedSampledImages::Rgba { image } | StagedSampledImages::Nv12 { image } => image,
                StagedSampledImages::YuvPlanes { luma, .. } => luma,
                StagedSampledImages::Bgra { .. } => {
                    unreachable!("NV12 output pool cannot contain a BGRA slot")
                }
            },
        }
    }

    pub fn device(&self) -> &D {
        match &self.kind {
            ImportedKind::DirectPacked(source) => match &source.source.allocation {
                PackedSourceAllocation::Direct(allocation) => allocation.device.as_ref(),
                PackedSourceAllocation::Staged { .. } => {
                    unreachable!("direct packed import owns a direct image")
                }
            },
            ImportedKind::DirectBgraScanout(sampled) | ImportedKind::DirectNv12(sampled, _) => {
                sampled.device.as_ref()
            }
            ImportedKind::StagedPacked(staged) => staged.output.slot.device.as_ref(),
            ImportedKind::StagedNv12(staged) => staged.output.slot.device.as_ref(),
        }
    }

    pub fn dimensions(&self) -> (u32, u32) {
        self.dimensions
    }

    pub fn modifier(&self) -> u64 {
        self.modifier
    }

    pub fn sampled_usage(&self) -> vk::ImageUsageFlags {
        self.sampled_usage
    }

    pub fn sampled_tiling(&self) -> vk::ImageTiling {
        self.sampled_tiling
    }

    pub fn sampled_format(&self) -> SampledImageFormat {
        match &self.kind {
            ImportedKind::DirectPacked(source) => match source.source.format {
                PackedImageFormat::Rgba8888 => SampledImageFormat::Rgba8888,
                PackedImageFormat::Bgra8888 => SampledImageFormat::Bgra8888,
            },
            ImportedKind::DirectBgraScanout(_) => SampledImageFormat::Bgra8888,
            ImportedKind::DirectNv12(_, _) => SampledImageFormat::Nv12,
            ImportedKind::StagedPacked(_) => SampledImageFormat::Bgra8888,
            ImportedKind::StagedNv12(staged) => match staged.output.slot.resources {
                StagedOutputResources::Rgba { .. } => SampledImageFormat::Rgba8888,
                StagedOutputResources::Nv12 { .. } => SampledImageFormat::Nv12,
                StagedOutputResources::TransferYuvPlanes { .. }
                | StagedOutputResources::YuvPlanes { .. } => SampledImageFormat::Nv12Planes,
            },
        }
    }

    pub fn nv12_sampling(&self) -> Option<DirectNv12Sampling> {
        match &self.kind {
            ImportedKind::DirectNv12(_, metadata) => Some(*metadata),
            ImportedKind::StagedNv12(staged)
                if matches!(
                    &staged.output.slot.resources,
                    StagedOutputResources::Nv12 { .. }
                ) =>
            {
                Some(DirectNv12Sampling {
                    conversion: staged.conversion,
                    format_features: staged.format_features,
                })
            }
            _ => None,
        }
    }

    pub fn direct_nv12_sampling(&self) -> Option<DirectNv12Sampling> {
        self.nv12_sampling()
    }

    pub fn staged_nv12_planes(&self) -> Option<StagedNv12Planes> {
        match &self.kind {
            ImportedKind::StagedNv12(staged) => match staged.output.slot.sampled_images() {
                StagedSampledImages::YuvPlanes { luma, chroma } => Some(StagedNv12Planes {
                    luma_image: luma,
                    chroma_image: chroma,
                    conversion: staged.conversion,
                }),
                StagedSampledImages::Rgba { .. }
                | StagedSampledImages::Bgra { .. }
                | StagedSampledImages::Nv12 { .. } => None,
            },
            _ => None,
        }
    }

    pub fn is_staged(&self) -> bool {
        matches!(
            self.kind,
            ImportedKind::StagedPacked(_) | ImportedKind::StagedNv12(_)
        )
    }

    pub fn release_staged_source(&self) -> bool {
        match &self.kind {
            ImportedKind::StagedPacked(staged) => staged.source.release(),
            ImportedKind::StagedNv12(staged) => staged.source.release(),
            _ => false,
        }
    }

    pub fn staged_source_released(&self) -> bool {
        match &self.kind {
            ImportedKind::StagedPacked(staged) => staged.source.is_released(),
            ImportedKind::StagedNv12(staged) => staged.source.is_released(),
            _ => false,
        }
    }

    pub(super) fn mark_acquire_submitted(&self) {
        match &self.kind {
            ImportedKind::StagedPacked(staged) => staged
                .output
                .slot
                .initialized
                .store(true, Ordering::Release),
            ImportedKind::StagedNv12(staged) => staged
                .output
                .slot
                .initialized
                .store(true, Ordering::Release),
            _ => {}
        }
    }

    pub(super) fn acquire_plan(&self) -> AcquirePlan {
        match &self.kind {
            ImportedKind::StagedPacked(staged) => {
                let (source_buffer, source_size) = match &staged.source.source.allocation {
                    PackedSourceAllocation::Staged { source, .. } => (source.buffer, source.size),
                    PackedSourceAllocation::Direct(_) => {
                        unreachable!("staged packed import owns a source buffer")
                    }
                };
                AcquirePlan::StagedCompute(StagedAcquirePlan {
                    source_buffer,
                    source_size,
                    output: StagedSampledImages::Bgra {
                        image: staged.output.slot.sampled.image,
                    },
                    output_initialized: staged.output.slot.initialized.load(Ordering::Acquire),
                    descriptor_set: staged.output.slot.descriptor_set,
                    pipeline: staged.pipeline.pipeline,
                    pipeline_layout: staged.pipeline.pipeline_layout,
                    push_constants: StagedPushConstants::Packed(staged.push_constants),
                    dispatch: (
                        self.dimensions.0.div_ceil(16),
                        self.dimensions.1.div_ceil(16),
                    ),
                })
            }
            ImportedKind::StagedNv12(staged) => {
                let source = staged.source.source.allocation.source();
                match &staged.operation {
                    StagedNv12Operation::Compute {
                        pipeline,
                        push_constants,
                    } => AcquirePlan::StagedCompute(StagedAcquirePlan {
                        source_buffer: source.buffer,
                        source_size: source.size,
                        output: staged.output.slot.sampled_images(),
                        output_initialized: staged.output.slot.initialized.load(Ordering::Acquire),
                        descriptor_set: staged
                            .output
                            .slot
                            .descriptor_set
                            .expect("compute NV12 output owns a descriptor set"),
                        pipeline: pipeline.pipeline,
                        pipeline_layout: pipeline.pipeline_layout,
                        push_constants: StagedPushConstants::Nv12(*push_constants),
                        dispatch: (
                            (self.dimensions.0 / 2).div_ceil(16),
                            (self.dimensions.1 / 2).div_ceil(16),
                        ),
                    }),
                    StagedNv12Operation::Transfer { layout } => {
                        AcquirePlan::StagedTransfer(StagedTransferPlan {
                            source_buffer: source.buffer,
                            source_size: source.size,
                            output: staged.output.slot.sampled_images(),
                            output_initialized: staged
                                .output
                                .slot
                                .initialized
                                .load(Ordering::Acquire),
                            dimensions: self.dimensions,
                            planes: layout.planes,
                        })
                    }
                }
            }
            ImportedKind::DirectPacked(source) => match &source.source.allocation {
                PackedSourceAllocation::Direct(allocation) => AcquirePlan::DirectImage {
                    image: allocation.image,
                },
                PackedSourceAllocation::Staged { .. } => {
                    unreachable!("direct packed import owns a direct image")
                }
            },
            ImportedKind::DirectBgraScanout(sampled) | ImportedKind::DirectNv12(sampled, _) => {
                AcquirePlan::DirectImage {
                    image: sampled.image,
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum AcquirePlan {
    DirectImage { image: vk::Image },
    StagedCompute(StagedAcquirePlan),
    StagedTransfer(StagedTransferPlan),
}

#[derive(Clone, Copy)]
pub(super) struct StagedAcquirePlan {
    pub source_buffer: vk::Buffer,
    pub source_size: u64,
    output: StagedSampledImages,
    pub output_initialized: bool,
    pub descriptor_set: vk::DescriptorSet,
    pub pipeline: vk::Pipeline,
    pub pipeline_layout: vk::PipelineLayout,
    pub push_constants: StagedPushConstants,
    pub dispatch: (u32, u32),
}

#[derive(Clone, Copy)]
pub(super) struct StagedTransferPlan {
    pub source_buffer: vk::Buffer,
    pub source_size: u64,
    output: StagedSampledImages,
    pub output_initialized: bool,
    pub dimensions: (u32, u32),
    pub planes: [ImportedPlane; 2],
}

#[derive(Clone, Copy)]
pub(super) enum StagedPushConstants {
    Nv12(Nv12PushConstants),
    Packed(PackedPushConstants),
}

impl StagedPushConstants {
    pub(super) fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Nv12(constants) => constants.as_bytes(),
            Self::Packed(constants) => constants.as_bytes(),
        }
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct PackedPushConstants {
    width: u32,
    height: u32,
    offset: u32,
    pitch: u32,
    source_format: u32,
}

impl PackedPushConstants {
    pub(super) fn as_bytes(&self) -> &[u8] {
        let pointer = std::ptr::from_ref(self).cast::<u8>();
        // SAFETY: the push-constant struct is repr(C), contains only u32 values, and the returned
        // slice cannot outlive the borrowed struct.
        unsafe { std::slice::from_raw_parts(pointer, std::mem::size_of::<Self>()) }
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
pub(super) struct Nv12PushConstants {
    width: u32,
    height: u32,
    y_offset: u32,
    y_pitch: u32,
    uv_offset: u32,
    uv_pitch: u32,
    range: u32,
    x_offset: u32,
    y_chroma_offset: u32,
}

impl Nv12PushConstants {
    pub(super) fn as_bytes(&self) -> &[u8] {
        let pointer = std::ptr::from_ref(self).cast::<u8>();
        // SAFETY: the push-constant struct is repr(C), contains only u32 values, and the returned
        // slice cannot outlive the borrowed struct.
        unsafe { std::slice::from_raw_parts(pointer, std::mem::size_of::<Self>()) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Nv12ComputeOutput {
    Rgba,
    YuvPlanes,
}

struct Nv12ComputePipeline<D: VulkanDeviceContext> {
    device: Arc<D>,
    output: Nv12ComputeOutput,
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
}

impl<D: VulkanDeviceContext> Nv12ComputePipeline<D> {
    fn new(device: Arc<D>, output: Nv12ComputeOutput) -> Result<Self, String> {
        let mut bindings = vec![
            vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_TEXEL_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE),
            vk::DescriptorSetLayoutBinding::default()
                .binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE),
        ];
        if output == Nv12ComputeOutput::YuvPlanes {
            bindings.push(
                vk::DescriptorSetLayoutBinding::default()
                    .binding(2)
                    .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE),
            );
        }
        let descriptor_info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
        let descriptor_set_layout = unsafe {
            device
                .device()
                .create_descriptor_set_layout(&descriptor_info, None)
        }
        .map_err(|result| format!("failed to create NV12 descriptor layout: {result:?}"))?;
        let result = (|| {
            let set_layouts = [descriptor_set_layout];
            let push_range = [vk::PushConstantRange::default()
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
                .offset(0)
                .size(
                    u32::try_from(std::mem::size_of::<Nv12PushConstants>())
                        .map_err(|_| "NV12 push-constant size exceeds u32".to_string())?,
                )];
            let pipeline_layout_info = vk::PipelineLayoutCreateInfo::default()
                .set_layouts(&set_layouts)
                .push_constant_ranges(&push_range);
            let pipeline_layout = unsafe {
                device
                    .device()
                    .create_pipeline_layout(&pipeline_layout_info, None)
            }
            .map_err(|result| format!("failed to create NV12 pipeline layout: {result:?}"))?;

            let words = match shader_words(output) {
                Ok(words) => words,
                Err(error) => {
                    unsafe {
                        device
                            .device()
                            .destroy_pipeline_layout(pipeline_layout, None)
                    };
                    return Err(error);
                }
            };
            let shader_info = vk::ShaderModuleCreateInfo::default().code(&words);
            let shader = match unsafe { device.device().create_shader_module(&shader_info, None) } {
                Ok(shader) => shader,
                Err(result) => {
                    unsafe {
                        device
                            .device()
                            .destroy_pipeline_layout(pipeline_layout, None)
                    };
                    return Err(format!("failed to create NV12 compute shader: {result:?}"));
                }
            };
            let name = c"main";
            let stage = vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::COMPUTE)
                .module(shader)
                .name(name);
            let pipeline_info = [vk::ComputePipelineCreateInfo::default()
                .stage(stage)
                .layout(pipeline_layout)];
            let pipeline_result = unsafe {
                device.device().create_compute_pipelines(
                    vk::PipelineCache::null(),
                    &pipeline_info,
                    None,
                )
            };
            unsafe { device.device().destroy_shader_module(shader, None) };
            let pipeline = match pipeline_result {
                Ok(mut pipelines) => match pipelines.pop() {
                    Some(pipeline) => pipeline,
                    None => {
                        unsafe {
                            device
                                .device()
                                .destroy_pipeline_layout(pipeline_layout, None)
                        };
                        return Err("Vulkan returned no NV12 compute pipeline".to_string());
                    }
                },
                Err((pipelines, result)) => {
                    unsafe {
                        pipelines
                            .into_iter()
                            .for_each(|pipeline| device.device().destroy_pipeline(pipeline, None));
                        device
                            .device()
                            .destroy_pipeline_layout(pipeline_layout, None);
                    }
                    return Err(format!(
                        "failed to create NV12 compute pipeline: {result:?}"
                    ));
                }
            };
            Ok(Self {
                device: Arc::clone(&device),
                output,
                descriptor_set_layout,
                pipeline_layout,
                pipeline,
            })
        })();
        if result.is_err() {
            unsafe {
                device
                    .device()
                    .destroy_descriptor_set_layout(descriptor_set_layout, None)
            };
        }
        result
    }
}

impl<D: VulkanDeviceContext> Drop for Nv12ComputePipeline<D> {
    fn drop(&mut self) {
        unsafe {
            self.device.device().destroy_pipeline(self.pipeline, None);
            self.device
                .device()
                .destroy_pipeline_layout(self.pipeline_layout, None);
            self.device
                .device()
                .destroy_descriptor_set_layout(self.descriptor_set_layout, None);
        }
    }
}

struct PackedComputePipeline<D: VulkanDeviceContext> {
    device: Arc<D>,
    descriptor_set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
}

impl<D: VulkanDeviceContext> PackedComputePipeline<D> {
    fn new(device: Arc<D>) -> Result<Self, String> {
        let bindings = [
            vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_TEXEL_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE),
            vk::DescriptorSetLayoutBinding::default()
                .binding(1)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE),
        ];
        let descriptor_info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
        let descriptor_set_layout = unsafe {
            device
                .device()
                .create_descriptor_set_layout(&descriptor_info, None)
        }
        .map_err(|result| format!("failed to create packed descriptor layout: {result:?}"))?;
        let result = (|| {
            let set_layouts = [descriptor_set_layout];
            let push_range = [vk::PushConstantRange::default()
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
                .offset(0)
                .size(
                    u32::try_from(std::mem::size_of::<PackedPushConstants>())
                        .map_err(|_| "packed push-constant size exceeds u32".to_string())?,
                )];
            let pipeline_layout_info = vk::PipelineLayoutCreateInfo::default()
                .set_layouts(&set_layouts)
                .push_constant_ranges(&push_range);
            let pipeline_layout = unsafe {
                device
                    .device()
                    .create_pipeline_layout(&pipeline_layout_info, None)
            }
            .map_err(|result| format!("failed to create packed pipeline layout: {result:?}"))?;

            let words = match packed_shader_words() {
                Ok(words) => words,
                Err(error) => {
                    unsafe {
                        device
                            .device()
                            .destroy_pipeline_layout(pipeline_layout, None)
                    };
                    return Err(error);
                }
            };
            let shader_info = vk::ShaderModuleCreateInfo::default().code(&words);
            let shader = match unsafe { device.device().create_shader_module(&shader_info, None) } {
                Ok(shader) => shader,
                Err(result) => {
                    unsafe {
                        device
                            .device()
                            .destroy_pipeline_layout(pipeline_layout, None)
                    };
                    return Err(format!(
                        "failed to create packed compute shader: {result:?}"
                    ));
                }
            };
            let stage = vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::COMPUTE)
                .module(shader)
                .name(c"main");
            let pipeline_info = [vk::ComputePipelineCreateInfo::default()
                .stage(stage)
                .layout(pipeline_layout)];
            let pipeline_result = unsafe {
                device.device().create_compute_pipelines(
                    vk::PipelineCache::null(),
                    &pipeline_info,
                    None,
                )
            };
            unsafe { device.device().destroy_shader_module(shader, None) };
            let pipeline = match pipeline_result {
                Ok(mut pipelines) => match pipelines.pop() {
                    Some(pipeline) => pipeline,
                    None => {
                        unsafe {
                            device
                                .device()
                                .destroy_pipeline_layout(pipeline_layout, None)
                        };
                        return Err("Vulkan returned no packed compute pipeline".to_string());
                    }
                },
                Err((pipelines, result)) => {
                    unsafe {
                        pipelines
                            .into_iter()
                            .for_each(|pipeline| device.device().destroy_pipeline(pipeline, None));
                        device
                            .device()
                            .destroy_pipeline_layout(pipeline_layout, None);
                    }
                    return Err(format!(
                        "failed to create packed compute pipeline: {result:?}"
                    ));
                }
            };
            Ok(Self {
                device: Arc::clone(&device),
                descriptor_set_layout,
                pipeline_layout,
                pipeline,
            })
        })();
        if result.is_err() {
            unsafe {
                device
                    .device()
                    .destroy_descriptor_set_layout(descriptor_set_layout, None)
            };
        }
        result
    }
}

impl<D: VulkanDeviceContext> Drop for PackedComputePipeline<D> {
    fn drop(&mut self) {
        unsafe {
            self.device.device().destroy_pipeline(self.pipeline, None);
            self.device
                .device()
                .destroy_pipeline_layout(self.pipeline_layout, None);
            self.device
                .device()
                .destroy_descriptor_set_layout(self.descriptor_set_layout, None);
        }
    }
}

fn packed_shader_words() -> Result<Vec<u32>, String> {
    let bytes = include_bytes!("packed_to_bgra.comp.spv");
    if !bytes.len().is_multiple_of(4) {
        return Err("embedded packed compute shader has an invalid byte length".to_string());
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
        .collect())
}

fn shader_words(output: Nv12ComputeOutput) -> Result<Vec<u32>, String> {
    let bytes: &[u8] = match output {
        Nv12ComputeOutput::Rgba => include_bytes!("nv12.comp.spv"),
        Nv12ComputeOutput::YuvPlanes => include_bytes!("nv12_planes.comp.spv"),
    };
    if !bytes.len().is_multiple_of(4) {
        return Err("embedded NV12 compute shader has an invalid byte length".to_string());
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
        .collect())
}

fn validate_staged_layout<D: VulkanDeviceContext>(
    device: &D,
    layout: Nv12SharedObjectLayout,
) -> Result<(), String> {
    if !layout.object_size.is_multiple_of(4) {
        return Err(
            "Vulkan staged NV12 allocation size must be four-byte aligned for shader access"
                .to_string(),
        );
    }
    let properties = unsafe {
        device
            .instance()
            .get_physical_device_properties(device.physical_device())
    };
    let source_texel_count = layout.object_size / 4;
    if source_texel_count > u64::from(properties.limits.max_texel_buffer_elements) {
        return Err(format!(
            "Vulkan staged NV12 allocation requires {source_texel_count} R32 texels, exceeding maxTexelBufferElements {}",
            properties.limits.max_texel_buffer_elements
        ));
    }
    Ok(())
}

fn validate_transfer_layout(
    dimensions: (u32, u32),
    layout: Nv12SharedObjectLayout,
) -> Result<(), String> {
    validate_nv12_shared_layout(dimensions, layout)?;
    if layout.object_size == 0 {
        return Err("Vulkan NV12 transfer source has a zero allocation size".to_string());
    }
    if layout
        .planes
        .iter()
        .any(|plane| !plane.offset.is_multiple_of(4))
    {
        return Err("Vulkan NV12 transfer plane offsets must be four-byte aligned".to_string());
    }
    if !layout.planes[1].pitch.is_multiple_of(2) {
        return Err(
            "Vulkan NV12 transfer chroma pitch must be divisible by two-byte texels".to_string(),
        );
    }
    Ok(())
}

fn validate_staged_packed_layout<D: VulkanDeviceContext>(
    device: &D,
    source_size: u64,
) -> Result<(), String> {
    if !source_size.is_multiple_of(4) {
        return Err(
            "Vulkan staged packed allocation size must be four-byte aligned for shader access"
                .to_string(),
        );
    }
    if source_size > u64::from(u32::MAX) + 1 {
        return Err(
            "Vulkan staged packed allocation exceeds the shader's 32-bit byte-address range"
                .to_string(),
        );
    }
    let properties = unsafe {
        device
            .instance()
            .get_physical_device_properties(device.physical_device())
    };
    let source_texel_count = source_size / 4;
    if source_texel_count > u64::from(properties.limits.max_texel_buffer_elements) {
        return Err(format!(
            "Vulkan staged packed allocation requires {source_texel_count} R32 texels, exceeding maxTexelBufferElements {}",
            properties.limits.max_texel_buffer_elements
        ));
    }
    Ok(())
}

fn dmabuf_identity(source_fd: i32) -> Result<(u64, u64), String> {
    if source_fd < 0 {
        return Err("Vulkan DMA-BUF identity requires a valid fd".to_string());
    }
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `stat` points to writable storage and `source_fd` remains borrowed by the caller.
    if unsafe { libc::fstat(source_fd, stat.as_mut_ptr()) } != 0 {
        return Err(format!(
            "failed to stat Vulkan DMA-BUF source fd: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: fstat initialized the complete structure after returning success.
    let stat = unsafe { stat.assume_init() };
    Ok((stat.st_dev, stat.st_ino))
}

fn create_cached_nv12_source<D: VulkanDeviceContext>(
    device: Arc<D>,
    source_fd: i32,
    source_size: u64,
    strategy: Nv12ImportStrategy,
) -> Result<CachedNv12Source<D>, String> {
    let allocation = match strategy {
        Nv12ImportStrategy::LinearBufferToOptimalNv12
        | Nv12ImportStrategy::LinearBufferToOptimalYuvPlanes => {
            CachedNv12SourceAllocation::Transfer {
                source: create_imported_buffer(
                    Arc::clone(&device),
                    source_fd,
                    source_size,
                    TRANSFER_NV12_SOURCE_USAGE,
                )?,
            }
        }
        Nv12ImportStrategy::LinearBufferToYuvPlanes | Nv12ImportStrategy::LinearBufferToRgba => {
            let source = create_imported_buffer(
                Arc::clone(&device),
                source_fd,
                source_size,
                STAGED_NV12_SOURCE_USAGE,
            )?;
            let source_view_info = vk::BufferViewCreateInfo::default()
                .buffer(source.buffer)
                .format(STAGED_NV12_SOURCE_TEXEL_FORMAT)
                .offset(0)
                .range(source_size);
            let view = BufferViewAllocation {
                device: Arc::clone(&device),
                view: unsafe { device.device().create_buffer_view(&source_view_info, None) }
                    .map_err(|result| {
                        format!("failed to create staged NV12 source view: {result:?}")
                    })?,
            };
            CachedNv12SourceAllocation::Compute { view, source }
        }
        Nv12ImportStrategy::DirectSampledImage => {
            return Err("direct NV12 image cannot be cached as a source buffer".to_string());
        }
    };
    Ok(CachedNv12Source {
        allocation,
        claimed: AtomicBool::new(false),
        last_used: AtomicU64::new(0),
    })
}

fn create_cached_packed_staged_source<D: VulkanDeviceContext>(
    device: Arc<D>,
    source_fd: i32,
    source_size: u64,
) -> Result<PackedSourceAllocation<D>, String> {
    let source = create_imported_buffer(
        Arc::clone(&device),
        source_fd,
        source_size,
        STAGED_PACKED_SOURCE_USAGE,
    )?;
    let source_view_info = vk::BufferViewCreateInfo::default()
        .buffer(source.buffer)
        .format(STAGED_PACKED_SOURCE_TEXEL_FORMAT)
        .offset(0)
        .range(source_size);
    let view = BufferViewAllocation {
        device: Arc::clone(&device),
        view: unsafe { device.device().create_buffer_view(&source_view_info, None) }
            .map_err(|result| format!("failed to create staged packed source view: {result:?}"))?,
    };
    Ok(PackedSourceAllocation::Staged { view, source })
}

fn create_image_view<D: VulkanDeviceContext>(
    device: Arc<D>,
    image: vk::Image,
    format: vk::Format,
) -> Result<ImageViewAllocation<D>, String> {
    let info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(format)
        .subresource_range(
            vk::ImageSubresourceRange::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .base_mip_level(0)
                .level_count(1)
                .base_array_layer(0)
                .layer_count(1),
        );
    let view = unsafe { device.device().create_image_view(&info, None) }
        .map_err(|result| format!("failed to create staged output view: {result:?}"))?;
    Ok(ImageViewAllocation { device, view })
}

fn create_staged_output_slot<D: VulkanDeviceContext>(
    device: Arc<D>,
    dimensions: (u32, u32),
    strategy: Nv12ImportStrategy,
    pipeline: Option<&Nv12ComputePipeline<D>>,
    source_view: Option<vk::BufferView>,
) -> Result<StagedOutputSlot<D>, String> {
    if matches!(
        strategy,
        Nv12ImportStrategy::LinearBufferToOptimalNv12
            | Nv12ImportStrategy::LinearBufferToOptimalYuvPlanes
    ) {
        if pipeline.is_some() || source_view.is_some() {
            return Err("Vulkan NV12 transfer output must not carry compute resources".to_string());
        }
        let resources = match strategy {
            Nv12ImportStrategy::LinearBufferToOptimalNv12 => {
                let sampled = create_local_image(
                    Arc::clone(&device),
                    dimensions,
                    IMPORTED_NV12_FORMAT,
                    TRANSFER_NV12_OUTPUT_USAGE,
                )?;
                StagedOutputResources::Nv12 { sampled }
            }
            Nv12ImportStrategy::LinearBufferToOptimalYuvPlanes => {
                let luma = create_local_image(
                    Arc::clone(&device),
                    dimensions,
                    STAGED_NV12_LUMA_FORMAT,
                    TRANSFER_NV12_OUTPUT_USAGE,
                )?;
                let chroma = create_local_image(
                    Arc::clone(&device),
                    (dimensions.0 / 2, dimensions.1 / 2),
                    STAGED_NV12_CHROMA_FORMAT,
                    TRANSFER_NV12_OUTPUT_USAGE,
                )?;
                StagedOutputResources::TransferYuvPlanes { luma, chroma }
            }
            _ => unreachable!("validated transfer strategy"),
        };
        return Ok(StagedOutputSlot {
            device,
            dimensions,
            strategy,
            descriptor_pool: None,
            descriptor_set: None,
            resources,
            claimed: AtomicBool::new(false),
            initialized: AtomicBool::new(false),
        });
    }
    let pipeline = pipeline
        .ok_or_else(|| "Vulkan compute NV12 output is missing its compute pipeline".to_string())?;
    let source_view = source_view.ok_or_else(|| {
        "Vulkan compute NV12 output is missing its uniform texel-buffer view".to_string()
    })?;
    let expected_output = match strategy {
        Nv12ImportStrategy::LinearBufferToRgba => Nv12ComputeOutput::Rgba,
        Nv12ImportStrategy::LinearBufferToYuvPlanes => Nv12ComputeOutput::YuvPlanes,
        Nv12ImportStrategy::DirectSampledImage => {
            return Err("direct NV12 import cannot allocate a staged output slot".to_string());
        }
        Nv12ImportStrategy::LinearBufferToOptimalNv12
        | Nv12ImportStrategy::LinearBufferToOptimalYuvPlanes => unreachable!("handled above"),
    };
    if pipeline.output != expected_output {
        return Err("Vulkan NV12 pipeline/output strategy mismatch".to_string());
    }

    let resources = match strategy {
        Nv12ImportStrategy::LinearBufferToRgba => {
            let sampled = create_local_image(
                Arc::clone(&device),
                dimensions,
                IMPORTED_RGBA_FORMAT,
                STAGED_NV12_OUTPUT_USAGE,
            )?;
            let view = create_image_view(Arc::clone(&device), sampled.image, IMPORTED_RGBA_FORMAT)?;
            StagedOutputResources::Rgba { view, sampled }
        }
        Nv12ImportStrategy::LinearBufferToYuvPlanes => {
            let luma = create_local_image(
                Arc::clone(&device),
                dimensions,
                STAGED_NV12_LUMA_FORMAT,
                STAGED_NV12_OUTPUT_USAGE,
            )?;
            let luma_view =
                create_image_view(Arc::clone(&device), luma.image, STAGED_NV12_LUMA_FORMAT)?;
            let chroma_dimensions = (dimensions.0 / 2, dimensions.1 / 2);
            let chroma = create_local_image(
                Arc::clone(&device),
                chroma_dimensions,
                STAGED_NV12_CHROMA_FORMAT,
                STAGED_NV12_OUTPUT_USAGE,
            )?;
            let chroma_view =
                create_image_view(Arc::clone(&device), chroma.image, STAGED_NV12_CHROMA_FORMAT)?;
            StagedOutputResources::YuvPlanes {
                luma_view,
                luma,
                chroma_view,
                chroma,
            }
        }
        Nv12ImportStrategy::DirectSampledImage => unreachable!("validated above"),
        Nv12ImportStrategy::LinearBufferToOptimalNv12
        | Nv12ImportStrategy::LinearBufferToOptimalYuvPlanes => unreachable!("handled above"),
    };

    let storage_count = if strategy == Nv12ImportStrategy::LinearBufferToYuvPlanes {
        2
    } else {
        1
    };
    let pool_sizes = [
        vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::UNIFORM_TEXEL_BUFFER)
            .descriptor_count(1),
        vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::STORAGE_IMAGE)
            .descriptor_count(storage_count),
    ];
    let pool_info = vk::DescriptorPoolCreateInfo::default()
        .max_sets(1)
        .pool_sizes(&pool_sizes);
    let descriptor_pool = unsafe { device.device().create_descriptor_pool(&pool_info, None) }
        .map_err(|result| format!("failed to create staged NV12 descriptor pool: {result:?}"))?;
    let set_layouts = [pipeline.descriptor_set_layout];
    let set_info = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(descriptor_pool)
        .set_layouts(&set_layouts);
    let descriptor_set = match unsafe { device.device().allocate_descriptor_sets(&set_info) } {
        Ok(sets) => sets
            .into_iter()
            .next()
            .ok_or_else(|| "Vulkan returned no staged NV12 descriptor set".to_string())?,
        Err(result) => {
            unsafe {
                device
                    .device()
                    .destroy_descriptor_pool(descriptor_pool, None)
            };
            return Err(format!(
                "failed to allocate staged NV12 descriptor set: {result:?}"
            ));
        }
    };
    let slot = StagedOutputSlot {
        device: Arc::clone(&device),
        dimensions,
        strategy,
        descriptor_pool: Some(descriptor_pool),
        descriptor_set: Some(descriptor_set),
        resources,
        claimed: AtomicBool::new(false),
        initialized: AtomicBool::new(false),
    };
    update_staged_descriptor_set(device.as_ref(), &slot, source_view)?;
    Ok(slot)
}

fn update_staged_descriptor_set<D: VulkanDeviceContext>(
    device: &D,
    slot: &StagedOutputSlot<D>,
    source_view: vk::BufferView,
) -> Result<(), String> {
    let source_views = [source_view];
    let mut image_infos = Vec::with_capacity(2);
    match &slot.resources {
        StagedOutputResources::Rgba { view, .. } => {
            image_infos.push(
                vk::DescriptorImageInfo::default()
                    .image_view(view.view)
                    .image_layout(vk::ImageLayout::GENERAL),
            );
        }
        StagedOutputResources::Nv12 { .. } | StagedOutputResources::TransferYuvPlanes { .. } => {
            return Err("Vulkan transfer NV12 output has no compute descriptor set".to_string());
        }
        StagedOutputResources::YuvPlanes {
            luma_view,
            chroma_view,
            ..
        } => {
            image_infos.push(
                vk::DescriptorImageInfo::default()
                    .image_view(luma_view.view)
                    .image_layout(vk::ImageLayout::GENERAL),
            );
            image_infos.push(
                vk::DescriptorImageInfo::default()
                    .image_view(chroma_view.view)
                    .image_layout(vk::ImageLayout::GENERAL),
            );
        }
    }
    let descriptor_set = slot
        .descriptor_set
        .ok_or_else(|| "Vulkan compute NV12 output is missing its descriptor set".to_string())?;
    let mut writes = vec![
        vk::WriteDescriptorSet::default()
            .dst_set(descriptor_set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::UNIFORM_TEXEL_BUFFER)
            .texel_buffer_view(&source_views),
        vk::WriteDescriptorSet::default()
            .dst_set(descriptor_set)
            .dst_binding(1)
            .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
            .image_info(&image_infos[0..1]),
    ];
    if image_infos.len() == 2 {
        writes.push(
            vk::WriteDescriptorSet::default()
                .dst_set(descriptor_set)
                .dst_binding(2)
                .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
                .image_info(&image_infos[1..2]),
        );
    }
    unsafe { device.device().update_descriptor_sets(&writes, &[]) };
    Ok(())
}

fn create_packed_output_slot<D: VulkanDeviceContext>(
    device: Arc<D>,
    dimensions: (u32, u32),
    pipeline: &PackedComputePipeline<D>,
    source_view: vk::BufferView,
) -> Result<PackedOutputSlot<D>, String> {
    let sampled = create_local_image_with_flags(
        Arc::clone(&device),
        dimensions,
        IMPORTED_SCANOUT_BGRA_FORMAT,
        STAGED_PACKED_OUTPUT_USAGE,
        vk::ImageCreateFlags::MUTABLE_FORMAT,
        &[
            IMPORTED_SCANOUT_BGRA_FORMAT,
            STAGED_PACKED_STORAGE_VIEW_FORMAT,
        ],
    )?;
    let storage_view = create_image_view(
        Arc::clone(&device),
        sampled.image,
        STAGED_PACKED_STORAGE_VIEW_FORMAT,
    )?;
    let pool_sizes = [
        vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::UNIFORM_TEXEL_BUFFER)
            .descriptor_count(1),
        vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::STORAGE_IMAGE)
            .descriptor_count(1),
    ];
    let pool_info = vk::DescriptorPoolCreateInfo::default()
        .max_sets(1)
        .pool_sizes(&pool_sizes);
    let descriptor_pool = unsafe { device.device().create_descriptor_pool(&pool_info, None) }
        .map_err(|result| format!("failed to create staged packed descriptor pool: {result:?}"))?;
    let set_layouts = [pipeline.descriptor_set_layout];
    let set_info = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(descriptor_pool)
        .set_layouts(&set_layouts);
    let descriptor_set = match unsafe { device.device().allocate_descriptor_sets(&set_info) } {
        Ok(sets) => match sets.into_iter().next() {
            Some(set) => set,
            None => {
                unsafe {
                    device
                        .device()
                        .destroy_descriptor_pool(descriptor_pool, None)
                };
                return Err("Vulkan returned no staged packed descriptor set".to_string());
            }
        },
        Err(result) => {
            unsafe {
                device
                    .device()
                    .destroy_descriptor_pool(descriptor_pool, None)
            };
            return Err(format!(
                "failed to allocate staged packed descriptor set: {result:?}"
            ));
        }
    };
    let slot = PackedOutputSlot {
        device: Arc::clone(&device),
        dimensions,
        descriptor_pool,
        descriptor_set,
        storage_view,
        sampled,
        claimed: AtomicBool::new(false),
        initialized: AtomicBool::new(false),
    };
    update_packed_descriptor_set(device.as_ref(), &slot, source_view);
    Ok(slot)
}

fn update_packed_descriptor_set<D: VulkanDeviceContext>(
    device: &D,
    slot: &PackedOutputSlot<D>,
    source_view: vk::BufferView,
) {
    let source_views = [source_view];
    let image_infos = [vk::DescriptorImageInfo::default()
        .image_view(slot.storage_view.view)
        .image_layout(vk::ImageLayout::GENERAL)];
    let writes = [
        vk::WriteDescriptorSet::default()
            .dst_set(slot.descriptor_set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::UNIFORM_TEXEL_BUFFER)
            .texel_buffer_view(&source_views),
        vk::WriteDescriptorSet::default()
            .dst_set(slot.descriptor_set)
            .dst_binding(1)
            .descriptor_type(vk::DescriptorType::STORAGE_IMAGE)
            .image_info(&image_infos),
    ];
    unsafe { device.device().update_descriptor_sets(&writes, &[]) };
}

fn create_staged_packed<D: VulkanDeviceContext>(
    request: PackedImageImport,
    source: PackedSourceLease<D>,
    output: PackedOutputLease<D>,
    pipeline: Arc<PackedComputePipeline<D>>,
) -> Result<ImportedDmaBufImage<D>, String> {
    let push_constants = PackedPushConstants {
        width: request.dimensions.0,
        height: request.dimensions.1,
        offset: u32::try_from(request.plane.offset)
            .map_err(|_| "Vulkan packed offset exceeds u32".to_string())?,
        pitch: request.plane.pitch,
        source_format: match request.format {
            PackedImageFormat::Rgba8888 => 0,
            PackedImageFormat::Bgra8888 => 1,
        },
    };
    Ok(ImportedDmaBufImage {
        kind: ImportedKind::StagedPacked(StagedPacked {
            source,
            output,
            pipeline,
            push_constants,
        }),
        dimensions: request.dimensions,
        modifier: request.modifier,
        sampled_usage: STAGED_PACKED_OUTPUT_USAGE,
        sampled_tiling: vk::ImageTiling::OPTIMAL,
    })
}

#[allow(clippy::too_many_arguments)]
fn create_staged_nv12<D: VulkanDeviceContext>(
    dimensions: (u32, u32),
    layout: Nv12SharedObjectLayout,
    conversion: Nv12Conversion,
    source: Nv12SourceLease<D>,
    output: Nv12OutputLease<D>,
    pipeline: Option<Arc<Nv12ComputePipeline<D>>>,
    strategy: Nv12ImportStrategy,
    sampled_format_features: vk::FormatFeatureFlags,
) -> Result<ImportedDmaBufImage<D>, String> {
    let operation = match strategy {
        Nv12ImportStrategy::LinearBufferToOptimalNv12
        | Nv12ImportStrategy::LinearBufferToOptimalYuvPlanes => {
            if pipeline.is_some() {
                return Err("Vulkan NV12 transfer unexpectedly owns a compute pipeline".to_string());
            }
            StagedNv12Operation::Transfer { layout }
        }
        Nv12ImportStrategy::LinearBufferToYuvPlanes | Nv12ImportStrategy::LinearBufferToRgba => {
            let pipeline = pipeline
                .ok_or_else(|| "Vulkan compute NV12 staging is missing its pipeline".to_string())?;
            let push_constants = Nv12PushConstants {
                width: dimensions.0,
                height: dimensions.1,
                y_offset: u32::try_from(layout.planes[0].offset)
                    .map_err(|_| "Vulkan NV12 luma offset exceeds u32".to_string())?,
                y_pitch: layout.planes[0].pitch,
                uv_offset: u32::try_from(layout.planes[1].offset)
                    .map_err(|_| "Vulkan NV12 chroma offset exceeds u32".to_string())?,
                uv_pitch: layout.planes[1].pitch,
                range: match conversion.range {
                    YcbcrRange::Narrow => 0,
                    YcbcrRange::Full => 1,
                },
                x_offset: match conversion.x_offset {
                    YcbcrOffset::CositedEven => 0,
                    YcbcrOffset::Midpoint => 1,
                },
                y_chroma_offset: match conversion.y_offset {
                    YcbcrOffset::CositedEven => 0,
                    YcbcrOffset::Midpoint => 1,
                },
            };
            StagedNv12Operation::Compute {
                pipeline,
                push_constants,
            }
        }
        Nv12ImportStrategy::DirectSampledImage => {
            return Err("direct NV12 import cannot be constructed as staged".to_string());
        }
    };
    let sampled_usage = if matches!(
        strategy,
        Nv12ImportStrategy::LinearBufferToOptimalNv12
            | Nv12ImportStrategy::LinearBufferToOptimalYuvPlanes
    ) {
        TRANSFER_NV12_OUTPUT_USAGE
    } else {
        STAGED_NV12_OUTPUT_USAGE
    };
    Ok(ImportedDmaBufImage {
        kind: ImportedKind::StagedNv12(StagedNv12 {
            source,
            output,
            operation,
            conversion,
            format_features: sampled_format_features,
        }),
        dimensions,
        modifier: layout.modifier,
        sampled_usage,
        sampled_tiling: vk::ImageTiling::OPTIMAL,
    })
}

fn validate_packed_layout(
    dimensions: (u32, u32),
    source_size: u64,
    plane: ImportedPlane,
) -> Result<(), String> {
    if source_size == 0 {
        return Err("Vulkan packed import has a zero allocation size".to_string());
    }
    let row_bytes = u64::from(dimensions.0)
        .checked_mul(4)
        .ok_or_else(|| "Vulkan packed row size overflow".to_string())?;
    if u64::from(plane.pitch) < row_bytes {
        return Err(format!(
            "Vulkan packed image pitch {} is smaller than row size {row_bytes}",
            plane.pitch
        ));
    }
    let required = plane
        .offset
        .checked_add(
            u64::from(plane.pitch)
                .checked_mul(u64::from(dimensions.1.saturating_sub(1)))
                .ok_or_else(|| "Vulkan packed image extent overflow".to_string())?,
        )
        .and_then(|last_row| last_row.checked_add(row_bytes))
        .ok_or_else(|| "Vulkan packed image extent overflow".to_string())?;
    if required > source_size {
        return Err(format!(
            "DMA-BUF object size {source_size} is smaller than packed image span {required}"
        ));
    }
    Ok(())
}

fn validate_image_inputs(
    dimensions: (u32, u32),
    source_fd: i32,
    plane: ImportedPlane,
) -> Result<(), String> {
    if dimensions.0 == 0 || dimensions.1 == 0 {
        return Err("Vulkan imported image dimensions must be non-zero".to_string());
    }
    if source_fd < 0 {
        return Err("Vulkan imported image has an invalid DMA-BUF fd".to_string());
    }
    if plane.pitch == 0 {
        return Err("Vulkan imported image has a zero row pitch".to_string());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn create_direct_image<D: VulkanDeviceContext>(
    device: Arc<D>,
    dimensions: (u32, u32),
    source_fd: i32,
    modifier: u64,
    planes: &[ImportedPlane],
    format: vk::Format,
    usage: vk::ImageUsageFlags,
    source_size: Option<u64>,
) -> Result<ImageAllocation<D>, String> {
    let plane_layouts = planes
        .iter()
        .map(|plane| {
            vk::SubresourceLayout::default()
                .offset(plane.offset)
                .row_pitch(u64::from(plane.pitch))
        })
        .collect::<Vec<_>>();
    let mut modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
        .drm_format_modifier(modifier)
        .plane_layouts(&plane_layouts);
    let mut external_info = vk::ExternalMemoryImageCreateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let create_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format)
        .extent(vk::Extent3D {
            width: dimensions.0,
            height: dimensions.1,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(usage)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut modifier_info)
        .push_next(&mut external_info);
    let image = unsafe { device.device().create_image(&create_info, None) }
        .map_err(|result| format!("failed to create Vulkan DMA-BUF import image: {result:?}"))?;
    match import_image_memory(Arc::clone(&device), image, source_fd, source_size) {
        Ok(memory) => Ok(ImageAllocation {
            device,
            image,
            memory,
        }),
        Err(error) => {
            unsafe { device.device().destroy_image(image, None) };
            Err(error)
        }
    }
}

fn import_image_memory<D: VulkanDeviceContext>(
    device: Arc<D>,
    image: vk::Image,
    source_fd: i32,
    source_size: Option<u64>,
) -> Result<vk::DeviceMemory, String> {
    let requirements_info = vk::ImageMemoryRequirementsInfo2::default().image(image);
    let mut dedicated_requirements = vk::MemoryDedicatedRequirements::default();
    let mut requirements =
        vk::MemoryRequirements2::default().push_next(&mut dedicated_requirements);
    unsafe {
        device
            .device()
            .get_image_memory_requirements2(&requirements_info, &mut requirements)
    };
    if source_size.is_some_and(|size| size < requirements.memory_requirements.size) {
        return Err(format!(
            "DMA-BUF object size {} is smaller than Vulkan import requirement {}",
            source_size.unwrap_or(0),
            requirements.memory_requirements.size
        ));
    }
    let memory_type_index = imported_memory_type(
        device.as_ref(),
        source_fd,
        requirements.memory_requirements.memory_type_bits,
    )?;
    let duplicate = duplicate_import_fd(source_fd)?;
    let raw_duplicate = duplicate.into_raw_fd();
    let mut import_info = vk::ImportMemoryFdInfoKHR::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .fd(raw_duplicate);
    let mut dedicated_info = vk::MemoryDedicatedAllocateInfo::default().image(image);
    let allocation_info = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.memory_requirements.size)
        .memory_type_index(memory_type_index)
        .push_next(&mut import_info)
        .push_next(&mut dedicated_info);
    let memory = match unsafe { device.device().allocate_memory(&allocation_info, None) } {
        Ok(memory) => memory,
        Err(result) => {
            close_unconsumed_fd(raw_duplicate);
            return Err(format!(
                "failed to import Vulkan DMA-BUF image memory: {result:?}"
            ));
        }
    };
    let bind = vk::BindImageMemoryInfo::default()
        .image(image)
        .memory(memory)
        .memory_offset(0);
    if let Err(result) = unsafe { device.device().bind_image_memory2(&[bind]) } {
        unsafe { device.device().free_memory(memory, None) };
        return Err(format!(
            "failed to bind imported Vulkan DMA-BUF image memory: {result:?}"
        ));
    }
    Ok(memory)
}

fn create_imported_buffer<D: VulkanDeviceContext>(
    device: Arc<D>,
    source_fd: i32,
    source_size: u64,
    usage: vk::BufferUsageFlags,
) -> Result<BufferAllocation<D>, String> {
    if source_fd < 0 || source_size == 0 {
        return Err("Vulkan DMA-BUF buffer import requires a valid fd and size".to_string());
    }
    let mut external = vk::ExternalMemoryBufferCreateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let info = vk::BufferCreateInfo::default()
        .size(source_size)
        .usage(usage)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .push_next(&mut external);
    let buffer = unsafe { device.device().create_buffer(&info, None) }
        .map_err(|result| format!("failed to create Vulkan DMA-BUF source buffer: {result:?}"))?;
    let requirements = unsafe { device.device().get_buffer_memory_requirements(buffer) };
    if requirements.size > source_size {
        unsafe { device.device().destroy_buffer(buffer, None) };
        return Err(format!(
            "DMA-BUF allocation size {source_size} is smaller than Vulkan source-buffer requirement {}",
            requirements.size
        ));
    }
    let memory_type_index =
        match imported_memory_type(device.as_ref(), source_fd, requirements.memory_type_bits) {
            Ok(index) => index,
            Err(error) => {
                unsafe { device.device().destroy_buffer(buffer, None) };
                return Err(error);
            }
        };
    let duplicate = match duplicate_import_fd(source_fd) {
        Ok(duplicate) => duplicate,
        Err(error) => {
            unsafe { device.device().destroy_buffer(buffer, None) };
            return Err(error);
        }
    };
    let raw_duplicate = duplicate.into_raw_fd();
    let mut import = vk::ImportMemoryFdInfoKHR::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .fd(raw_duplicate);
    let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().buffer(buffer);
    let allocation = vk::MemoryAllocateInfo::default()
        .allocation_size(source_size)
        .memory_type_index(memory_type_index)
        .push_next(&mut import)
        .push_next(&mut dedicated);
    let memory = match unsafe { device.device().allocate_memory(&allocation, None) } {
        Ok(memory) => memory,
        Err(result) => {
            close_unconsumed_fd(raw_duplicate);
            unsafe { device.device().destroy_buffer(buffer, None) };
            return Err(format!(
                "failed to import Vulkan DMA-BUF source memory: {result:?}"
            ));
        }
    };
    if let Err(result) = unsafe { device.device().bind_buffer_memory(buffer, memory, 0) } {
        unsafe {
            device.device().free_memory(memory, None);
            device.device().destroy_buffer(buffer, None);
        }
        return Err(format!(
            "failed to bind imported Vulkan DMA-BUF source memory: {result:?}"
        ));
    }
    Ok(BufferAllocation {
        device,
        buffer,
        memory,
        size: source_size,
    })
}

fn create_local_image<D: VulkanDeviceContext>(
    device: Arc<D>,
    dimensions: (u32, u32),
    format: vk::Format,
    usage: vk::ImageUsageFlags,
) -> Result<ImageAllocation<D>, String> {
    create_local_image_with_flags(
        device,
        dimensions,
        format,
        usage,
        vk::ImageCreateFlags::empty(),
        &[],
    )
}

fn create_local_image_with_flags<D: VulkanDeviceContext>(
    device: Arc<D>,
    dimensions: (u32, u32),
    format: vk::Format,
    usage: vk::ImageUsageFlags,
    flags: vk::ImageCreateFlags,
    view_formats: &[vk::Format],
) -> Result<ImageAllocation<D>, String> {
    let mut format_list = vk::ImageFormatListCreateInfo::default().view_formats(view_formats);
    let base_info = vk::ImageCreateInfo::default()
        .flags(flags)
        .image_type(vk::ImageType::TYPE_2D)
        .format(format)
        .extent(vk::Extent3D {
            width: dimensions.0,
            height: dimensions.1,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(usage)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let info = if view_formats.is_empty() {
        base_info
    } else {
        base_info.push_next(&mut format_list)
    };
    let image = unsafe { device.device().create_image(&info, None) }
        .map_err(|result| format!("failed to create staged output image: {result:?}"))?;
    let requirements = unsafe { device.device().get_image_memory_requirements(image) };
    let memory_type_index = match select_memory_type(device.as_ref(), requirements.memory_type_bits)
    {
        Ok(index) => index,
        Err(error) => {
            unsafe { device.device().destroy_image(image, None) };
            return Err(error);
        }
    };
    let allocation = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(memory_type_index);
    let memory = match unsafe { device.device().allocate_memory(&allocation, None) } {
        Ok(memory) => memory,
        Err(result) => {
            unsafe { device.device().destroy_image(image, None) };
            return Err(format!(
                "failed to allocate staged output memory: {result:?}"
            ));
        }
    };
    if let Err(result) = unsafe { device.device().bind_image_memory(image, memory, 0) } {
        unsafe {
            device.device().free_memory(memory, None);
            device.device().destroy_image(image, None);
        }
        return Err(format!("failed to bind staged output memory: {result:?}"));
    }
    Ok(ImageAllocation {
        device,
        image,
        memory,
    })
}

fn duplicate_import_fd(source_fd: i32) -> Result<OwnedFd, String> {
    duplicate_fd_cloexec(source_fd)
        .map_err(|error| format!("failed to duplicate Vulkan DMA-BUF import fd: {error}"))
}

fn close_unconsumed_fd(raw_fd: i32) {
    // SAFETY: Vulkan did not accept ownership after a failed allocation/import call.
    unsafe { libc::close(raw_fd) };
}

fn imported_memory_type<D: VulkanDeviceContext>(
    device: &D,
    source_fd: i32,
    requirement_bits: u32,
) -> Result<u32, String> {
    let loader = ash::khr::external_memory_fd::Device::new(device.instance(), device.device());
    let mut properties = vk::MemoryFdPropertiesKHR::default();
    unsafe {
        loader.get_memory_fd_properties(
            vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
            source_fd,
            &mut properties,
        )
    }
    .map_err(|result| format!("failed to query Vulkan DMA-BUF memory properties: {result:?}"))?;
    select_memory_type(device, requirement_bits & properties.memory_type_bits)
}

fn select_memory_type<D: VulkanDeviceContext>(device: &D, bits: u32) -> Result<u32, String> {
    let properties = unsafe {
        device
            .instance()
            .get_physical_device_memory_properties(device.physical_device())
    };
    properties.memory_types[..usize::try_from(properties.memory_type_count).unwrap_or(0)]
        .iter()
        .enumerate()
        .find(|(index, _memory_type)| bits & (1_u32 << index) != 0)
        .map(|(index, _memory_type)| index as u32)
        .ok_or_else(|| "imported Vulkan DMA-BUF has no compatible memory type".to_string())
}

pub fn validate_bgra_scanout_import_support<D: VulkanDeviceContext>(
    device: &D,
    modifier: u64,
    usage: vk::ImageUsageFlags,
) -> Result<(), String> {
    validate_direct_modifier(
        device,
        IMPORTED_SCANOUT_BGRA_FORMAT,
        modifier,
        usage,
        vk::FormatFeatureFlags::COLOR_ATTACHMENT
            | vk::FormatFeatureFlags::TRANSFER_SRC
            | vk::FormatFeatureFlags::TRANSFER_DST,
        "B8G8R8A8",
    )
}

pub fn validate_packed_import_support<D: VulkanDeviceContext>(
    device: &D,
    format: PackedImageFormat,
    modifier: u64,
) -> Result<(), String> {
    validate_direct_modifier(
        device,
        format.vk_format(),
        modifier,
        DIRECT_PACKED_USAGE,
        vk::FormatFeatureFlags::SAMPLED_IMAGE
            | vk::FormatFeatureFlags::TRANSFER_SRC
            | vk::FormatFeatureFlags::TRANSFER_DST
            | vk::FormatFeatureFlags::COLOR_ATTACHMENT,
        format.label(),
    )
}

pub fn validate_packed_staging_support<D: VulkanDeviceContext>(
    device: &D,
    format: PackedImageFormat,
    modifier: u64,
) -> Result<(), String> {
    if modifier != DRM_FORMAT_MOD_LINEAR {
        return Err(format!(
            "Vulkan packed buffer staging requires linear modifier 0, got {modifier:#018x}"
        ));
    }
    if !selected_queue_supports_compute(device) {
        return Err("selected Vulkan queue does not support packed compute staging".to_string());
    }
    let external_info = vk::PhysicalDeviceExternalBufferInfo::default()
        .flags(vk::BufferCreateFlags::empty())
        .usage(STAGED_PACKED_SOURCE_USAGE)
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let mut external = vk::ExternalBufferProperties::default();
    unsafe {
        device
            .instance()
            .get_physical_device_external_buffer_properties(
                device.physical_device(),
                &external_info,
                &mut external,
            )
    };
    let external = external.external_memory_properties;
    validate_external_import(
        external.external_memory_features,
        external.compatible_handle_types,
        &format!("linear {} source buffer", format.label()),
    )?;

    let mut source_properties = vk::FormatProperties2::default();
    unsafe {
        device.instance().get_physical_device_format_properties2(
            device.physical_device(),
            STAGED_PACKED_SOURCE_TEXEL_FORMAT,
            &mut source_properties,
        )
    };
    if !source_properties
        .format_properties
        .buffer_features
        .contains(vk::FormatFeatureFlags::UNIFORM_TEXEL_BUFFER)
    {
        return Err("Vulkan R32_UINT packed source lacks uniform-texel-buffer support".to_string());
    }

    let output_required = vk::FormatFeatureFlags::SAMPLED_IMAGE
        | vk::FormatFeatureFlags::STORAGE_IMAGE
        | vk::FormatFeatureFlags::TRANSFER_SRC
        | vk::FormatFeatureFlags::TRANSFER_DST;
    let (_output_features, _max_extent) = query_optimal_staging_format_with_usage(
        device,
        IMPORTED_SCANOUT_BGRA_FORMAT,
        output_required,
        STAGED_PACKED_OUTPUT_USAGE,
        vk::ImageCreateFlags::MUTABLE_FORMAT,
    )?;
    let mut storage_properties = vk::FormatProperties2::default();
    unsafe {
        device.instance().get_physical_device_format_properties2(
            device.physical_device(),
            STAGED_PACKED_STORAGE_VIEW_FORMAT,
            &mut storage_properties,
        )
    };
    if !storage_properties
        .format_properties
        .optimal_tiling_features
        .contains(vk::FormatFeatureFlags::STORAGE_IMAGE)
    {
        return Err("Vulkan R32_UINT packed output view lacks storage-image support".to_string());
    }
    Ok(())
}

pub fn validate_rgba_import_support<D: VulkanDeviceContext>(
    device: &D,
    modifier: u64,
) -> Result<(), String> {
    validate_packed_import_support(device, PackedImageFormat::Rgba8888, modifier)
}

fn validate_direct_modifier<D: VulkanDeviceContext>(
    device: &D,
    format: vk::Format,
    modifier: u64,
    usage: vk::ImageUsageFlags,
    required: vk::FormatFeatureFlags,
    label: &str,
) -> Result<(), String> {
    let modifiers = format_modifiers(device, format)?;
    if !modifiers.iter().any(|candidate| {
        candidate.drm_format_modifier == modifier
            && candidate.drm_format_modifier_plane_count == 1
            && candidate
                .drm_format_modifier_tiling_features
                .contains(required)
    }) {
        return Err(format!(
            "Vulkan device cannot use one-plane {label} DMA-BUF modifier {modifier:#018x}"
        ));
    }
    let external = query_external_modifier_capability(device, format, modifier, usage)?;
    validate_external_import(
        external.external_features,
        external.compatible_handle_types,
        label,
    )
}

pub fn inventory_nv12_modifier_capabilities<D: VulkanDeviceContext>(
    device: &D,
) -> Result<Vec<Nv12ModifierCapability>, String> {
    inventory_nv12_modifier_capabilities_with_staging_preference(
        device,
        Nv12StagingPreference::default(),
    )
}

pub fn inventory_nv12_modifier_capabilities_with_staging_preference<D: VulkanDeviceContext>(
    device: &D,
    staging_preference: Nv12StagingPreference,
) -> Result<Vec<Nv12ModifierCapability>, String> {
    let modifiers = format_modifiers(device, IMPORTED_NV12_FORMAT)?;
    let direct = modifiers.iter().filter_map(|modifier| {
        query_external_modifier_capability(
            device,
            IMPORTED_NV12_FORMAT,
            modifier.drm_format_modifier,
            DIRECT_NV12_USAGE,
        )
        .ok()
        .filter(|external| {
            modifier.drm_format_modifier_plane_count == 2
                && external
                    .external_features
                    .contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE)
                && external
                    .compatible_handle_types
                    .contains(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        })
        .map(|external| Nv12ModifierCapability {
            modifier: modifier.drm_format_modifier,
            strategy: Nv12ImportStrategy::DirectSampledImage,
            modifier_plane_count: modifier.drm_format_modifier_plane_count,
            source_tiling_features: modifier.drm_format_modifier_tiling_features,
            sampled_tiling_features: modifier.drm_format_modifier_tiling_features,
            external_features: external.external_features,
            compatible_handle_types: external.compatible_handle_types,
            max_extent: external.max_extent,
        })
    });
    let mut capabilities = direct.collect::<Vec<_>>();
    if !capabilities
        .iter()
        .any(|capability| capability.modifier == DRM_FORMAT_MOD_LINEAR)
        && let Some(linear) = modifiers
            .iter()
            .find(|modifier| modifier.drm_format_modifier == DRM_FORMAT_MOD_LINEAR)
        && let Ok(staged) =
            query_linear_nv12_staging_capability(device, *linear, staging_preference)
    {
        capabilities.push(staged);
    }
    Ok(capabilities)
}

fn selected_queue_flags<D: VulkanDeviceContext>(device: &D) -> vk::QueueFlags {
    unsafe {
        device
            .instance()
            .get_physical_device_queue_family_properties(device.physical_device())
    }
    .get(usize::try_from(device.queue_family_index()).unwrap_or(usize::MAX))
    .filter(|properties| properties.queue_count > 0)
    .map(|properties| properties.queue_flags)
    .unwrap_or_default()
}

fn selected_queue_supports_compute<D: VulkanDeviceContext>(device: &D) -> bool {
    selected_queue_flags(device).contains(vk::QueueFlags::COMPUTE)
}

fn query_external_buffer_properties<D: VulkanDeviceContext>(
    device: &D,
    usage: vk::BufferUsageFlags,
    label: &str,
) -> Result<vk::ExternalMemoryProperties, String> {
    let external_info = vk::PhysicalDeviceExternalBufferInfo::default()
        .flags(vk::BufferCreateFlags::empty())
        .usage(usage)
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let mut external = vk::ExternalBufferProperties::default();
    unsafe {
        device
            .instance()
            .get_physical_device_external_buffer_properties(
                device.physical_device(),
                &external_info,
                &mut external,
            )
    };
    let properties = external.external_memory_properties;
    validate_external_import(
        properties.external_memory_features,
        properties.compatible_handle_types,
        label,
    )?;
    Ok(properties)
}

fn query_linear_nv12_staging_capability<D: VulkanDeviceContext>(
    device: &D,
    linear: vk::DrmFormatModifierPropertiesEXT,
    staging_preference: Nv12StagingPreference,
) -> Result<Nv12ModifierCapability, String> {
    match staging_preference {
        Nv12StagingPreference::PreferPlanar => {
            match query_linear_nv12_transfer_capability(device, linear) {
                Ok(capability) => Ok(capability),
                Err(transfer_error) => query_linear_nv12_compute_capability(
                    device,
                    linear,
                    Nv12StagingPreference::PreferPlanar,
                )
                .map_err(|compute_error| {
                    format!(
                        "Vulkan NV12 has neither optimal transfer nor compute staging support: transfer={transfer_error}; compute={compute_error}"
                    )
                }),
            }
        }
        Nv12StagingPreference::RequirePlanar | Nv12StagingPreference::RequireRgba => {
            query_linear_nv12_compute_capability(device, linear, staging_preference)
        }
    }
}

fn query_linear_nv12_transfer_capability<D: VulkanDeviceContext>(
    device: &D,
    linear: vk::DrmFormatModifierPropertiesEXT,
) -> Result<Nv12ModifierCapability, String> {
    let queue_flags = selected_queue_flags(device);
    if !queue_flags
        .intersects(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER)
    {
        return Err("selected Vulkan queue cannot execute buffer-to-image transfers".to_string());
    }
    let external = query_external_buffer_properties(
        device,
        TRANSFER_NV12_SOURCE_USAGE,
        "linear NV12 transfer-source buffer",
    )?;
    let multi_planar_required = vk::FormatFeatureFlags::SAMPLED_IMAGE
        | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR
        | vk::FormatFeatureFlags::SAMPLED_IMAGE_YCBCR_CONVERSION_LINEAR_FILTER
        | vk::FormatFeatureFlags::MIDPOINT_CHROMA_SAMPLES
        | vk::FormatFeatureFlags::COSITED_CHROMA_SAMPLES
        | vk::FormatFeatureFlags::TRANSFER_SRC
        | vk::FormatFeatureFlags::TRANSFER_DST;
    let (strategy, sampled_features, max_extent) = match query_optimal_staging_format_with_usage(
        device,
        IMPORTED_NV12_FORMAT,
        multi_planar_required,
        TRANSFER_NV12_OUTPUT_USAGE,
        vk::ImageCreateFlags::empty(),
    ) {
        Ok((features, extent)) => (
            Nv12ImportStrategy::LinearBufferToOptimalNv12,
            features,
            extent,
        ),
        Err(multi_planar_error) => {
            let required = vk::FormatFeatureFlags::SAMPLED_IMAGE
                | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR
                | vk::FormatFeatureFlags::TRANSFER_SRC
                | vk::FormatFeatureFlags::TRANSFER_DST;
            let (features, extent) = query_transfer_planar_staging_format(device, required)
                    .map_err(|planar_error| {
                        format!(
                            "Vulkan NV12 has neither exact multi-planar YCbCr nor separate-plane transfer support: multi_planar={multi_planar_error}; separate_planes={planar_error}"
                        )
                    })?;
            (
                Nv12ImportStrategy::LinearBufferToOptimalYuvPlanes,
                features,
                extent,
            )
        }
    };
    Ok(Nv12ModifierCapability {
        modifier: DRM_FORMAT_MOD_LINEAR,
        strategy,
        modifier_plane_count: linear.drm_format_modifier_plane_count,
        source_tiling_features: linear.drm_format_modifier_tiling_features,
        sampled_tiling_features: sampled_features,
        external_features: external.external_memory_features,
        compatible_handle_types: external.compatible_handle_types,
        max_extent,
    })
}

fn query_linear_nv12_compute_capability<D: VulkanDeviceContext>(
    device: &D,
    linear: vk::DrmFormatModifierPropertiesEXT,
    staging_preference: Nv12StagingPreference,
) -> Result<Nv12ModifierCapability, String> {
    if !selected_queue_flags(device).contains(vk::QueueFlags::COMPUTE) {
        return Err("selected Vulkan queue cannot execute NV12 compute staging".to_string());
    }
    let external = query_external_buffer_properties(
        device,
        STAGED_NV12_SOURCE_USAGE,
        "linear NV12 uniform-texel source buffer",
    )?;

    let mut source_properties = vk::FormatProperties2::default();
    unsafe {
        device.instance().get_physical_device_format_properties2(
            device.physical_device(),
            STAGED_NV12_SOURCE_TEXEL_FORMAT,
            &mut source_properties,
        )
    };
    if !source_properties
        .format_properties
        .buffer_features
        .contains(vk::FormatFeatureFlags::UNIFORM_TEXEL_BUFFER)
    {
        return Err("Vulkan R32_UINT source buffers lack uniform-texel-buffer support".to_string());
    }

    let required = vk::FormatFeatureFlags::SAMPLED_IMAGE
        | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR
        | vk::FormatFeatureFlags::STORAGE_IMAGE
        | vk::FormatFeatureFlags::TRANSFER_SRC
        | vk::FormatFeatureFlags::TRANSFER_DST;
    let (strategy, sampled_features, max_extent) = match staging_preference {
        Nv12StagingPreference::PreferPlanar => {
            match query_planar_staging_format(device, required) {
                Ok((features, extent)) => (
                    Nv12ImportStrategy::LinearBufferToYuvPlanes,
                    features,
                    extent,
                ),
                Err(planar_error) => {
                    let (features, extent) =
                        query_optimal_staging_format(device, IMPORTED_RGBA_FORMAT, required).map_err(
                            |rgba_error| {
                                format!(
                                    "Vulkan NV12 has neither planar nor RGBA compute staging support: planar={planar_error}; rgba={rgba_error}"
                                )
                            },
                        )?;
                    (Nv12ImportStrategy::LinearBufferToRgba, features, extent)
                }
            }
        }
        Nv12StagingPreference::RequirePlanar => {
            let (features, extent) = query_planar_staging_format(device, required)
                .map_err(|error| format!("required planar NV12 staging is unavailable: {error}"))?;
            (
                Nv12ImportStrategy::LinearBufferToYuvPlanes,
                features,
                extent,
            )
        }
        Nv12StagingPreference::RequireRgba => {
            let (features, extent) =
                query_optimal_staging_format(device, IMPORTED_RGBA_FORMAT, required).map_err(
                    |error| format!("required RGBA NV12 staging is unavailable: {error}"),
                )?;
            (Nv12ImportStrategy::LinearBufferToRgba, features, extent)
        }
    };

    Ok(Nv12ModifierCapability {
        modifier: DRM_FORMAT_MOD_LINEAR,
        strategy,
        modifier_plane_count: linear.drm_format_modifier_plane_count,
        source_tiling_features: linear.drm_format_modifier_tiling_features,
        sampled_tiling_features: sampled_features,
        external_features: external.external_memory_features,
        compatible_handle_types: external.compatible_handle_types,
        max_extent,
    })
}

fn query_transfer_planar_staging_format<D: VulkanDeviceContext>(
    device: &D,
    required: vk::FormatFeatureFlags,
) -> Result<(vk::FormatFeatureFlags, vk::Extent3D), String> {
    query_optimal_staging_format_with_usage(
        device,
        STAGED_NV12_LUMA_FORMAT,
        required,
        TRANSFER_NV12_OUTPUT_USAGE,
        vk::ImageCreateFlags::empty(),
    )
    .and_then(|(luma_features, luma_extent)| {
        query_optimal_staging_format_with_usage(
            device,
            STAGED_NV12_CHROMA_FORMAT,
            required,
            TRANSFER_NV12_OUTPUT_USAGE,
            vk::ImageCreateFlags::empty(),
        )
        .map(|(chroma_features, chroma_extent)| {
            (
                luma_features & chroma_features,
                vk::Extent3D {
                    width: luma_extent.width.min(chroma_extent.width.saturating_mul(2)),
                    height: luma_extent
                        .height
                        .min(chroma_extent.height.saturating_mul(2)),
                    depth: 1,
                },
            )
        })
    })
}

fn query_planar_staging_format<D: VulkanDeviceContext>(
    device: &D,
    required: vk::FormatFeatureFlags,
) -> Result<(vk::FormatFeatureFlags, vk::Extent3D), String> {
    query_optimal_staging_format(device, STAGED_NV12_LUMA_FORMAT, required).and_then(
        |(luma_features, luma_extent)| {
            query_optimal_staging_format(device, STAGED_NV12_CHROMA_FORMAT, required).map(
                |(chroma_features, chroma_extent)| {
                    (
                        luma_features & chroma_features,
                        vk::Extent3D {
                            width: luma_extent.width.min(chroma_extent.width.saturating_mul(2)),
                            height: luma_extent
                                .height
                                .min(chroma_extent.height.saturating_mul(2)),
                            depth: 1,
                        },
                    )
                },
            )
        },
    )
}

fn query_optimal_staging_format<D: VulkanDeviceContext>(
    device: &D,
    format: vk::Format,
    required: vk::FormatFeatureFlags,
) -> Result<(vk::FormatFeatureFlags, vk::Extent3D), String> {
    query_optimal_staging_format_with_usage(
        device,
        format,
        required,
        STAGED_NV12_OUTPUT_USAGE,
        vk::ImageCreateFlags::empty(),
    )
}

fn query_optimal_staging_format_with_usage<D: VulkanDeviceContext>(
    device: &D,
    format: vk::Format,
    required: vk::FormatFeatureFlags,
    usage: vk::ImageUsageFlags,
    flags: vk::ImageCreateFlags,
) -> Result<(vk::FormatFeatureFlags, vk::Extent3D), String> {
    let mut properties = vk::FormatProperties2::default();
    unsafe {
        device.instance().get_physical_device_format_properties2(
            device.physical_device(),
            format,
            &mut properties,
        )
    };
    let features = properties.format_properties.optimal_tiling_features;
    if !features.contains(required) {
        return Err(format!(
            "optimal format 0x{:x} lacks required features 0x{:x} (available=0x{:x})",
            format.as_raw(),
            required.as_raw(),
            features.as_raw()
        ));
    }
    let format_info = vk::PhysicalDeviceImageFormatInfo2::default()
        .format(format)
        .ty(vk::ImageType::TYPE_2D)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(usage)
        .flags(flags);
    let mut image_properties = vk::ImageFormatProperties2::default();
    unsafe {
        device
            .instance()
            .get_physical_device_image_format_properties2(
                device.physical_device(),
                &format_info,
                &mut image_properties,
            )
    }
    .map_err(|result| {
        format!(
            "Vulkan optimal staging format 0x{:x} is unsupported: {result:?}",
            format.as_raw()
        )
    })?;
    Ok((
        features,
        image_properties.image_format_properties.max_extent,
    ))
}

fn validate_external_import(
    features: vk::ExternalMemoryFeatureFlags,
    handles: vk::ExternalMemoryHandleTypeFlags,
    label: &str,
) -> Result<(), String> {
    if !features.contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE)
        || !handles.contains(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
    {
        return Err(format!("{label} is not Vulkan DMA-BUF importable"));
    }
    Ok(())
}

fn format_modifiers<D: VulkanDeviceContext>(
    device: &D,
    format: vk::Format,
) -> Result<Vec<vk::DrmFormatModifierPropertiesEXT>, String> {
    let mut count = vk::DrmFormatModifierPropertiesListEXT::default();
    let mut properties = vk::FormatProperties2::default().push_next(&mut count);
    unsafe {
        device.instance().get_physical_device_format_properties2(
            device.physical_device(),
            format,
            &mut properties,
        )
    };
    let mut modifiers = vec![
        vk::DrmFormatModifierPropertiesEXT::default();
        usize::try_from(count.drm_format_modifier_count).map_err(|_| {
            "Vulkan DRM modifier count exceeds usize".to_string()
        })?
    ];
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default()
        .drm_format_modifier_properties(&mut modifiers);
    let mut properties = vk::FormatProperties2::default().push_next(&mut list);
    unsafe {
        device.instance().get_physical_device_format_properties2(
            device.physical_device(),
            format,
            &mut properties,
        )
    };
    Ok(modifiers)
}

struct ExternalModifierCapability {
    external_features: vk::ExternalMemoryFeatureFlags,
    compatible_handle_types: vk::ExternalMemoryHandleTypeFlags,
    max_extent: vk::Extent3D,
}

fn query_external_modifier_capability<D: VulkanDeviceContext>(
    device: &D,
    format: vk::Format,
    modifier: u64,
    usage: vk::ImageUsageFlags,
) -> Result<ExternalModifierCapability, String> {
    let mut modifier_info = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
        .drm_format_modifier(modifier)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    let mut external_info = vk::PhysicalDeviceExternalImageFormatInfo::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let format_info = vk::PhysicalDeviceImageFormatInfo2::default()
        .format(format)
        .ty(vk::ImageType::TYPE_2D)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(usage)
        .flags(vk::ImageCreateFlags::empty())
        .push_next(&mut modifier_info)
        .push_next(&mut external_info);
    let mut external_properties = vk::ExternalImageFormatProperties::default();
    let max_extent = {
        let mut image_properties =
            vk::ImageFormatProperties2::default().push_next(&mut external_properties);
        unsafe {
            device.instance().get_physical_device_image_format_properties2(
                device.physical_device(),
                &format_info,
                &mut image_properties,
            )
        }
        .map_err(|result| {
            format!(
                "Vulkan DMA-BUF modifier {modifier:#018x} is not importable for format 0x{:x}: {result:?}",
                format.as_raw()
            )
        })?;
        image_properties.image_format_properties.max_extent
    };
    let external_memory = external_properties.external_memory_properties;
    Ok(ExternalModifierCapability {
        external_features: external_memory.external_memory_features,
        compatible_handle_types: external_memory.compatible_handle_types,
        max_extent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color(chroma_location: ChromaLocation) -> Colorimetry {
        Colorimetry {
            primaries: Primaries::Bt709,
            transfer: Transfer::Bt709,
            matrix: Matrix::Bt709,
            range: ColorRange::Limited,
            chroma_location,
        }
    }

    #[test]
    fn packed_formats_map_to_exact_vulkan_byte_orders_and_topology_keys() {
        assert_eq!(
            PackedImageFormat::Rgba8888.vk_format().as_raw(),
            vk::Format::R8G8B8A8_UNORM.as_raw()
        );
        assert_eq!(
            PackedImageFormat::Bgra8888.vk_format().as_raw(),
            vk::Format::B8G8R8A8_UNORM.as_raw()
        );
        let base = PackedSourceTopology {
            dimensions: (64, 32),
            object_size: 8_192,
            modifier: DRM_FORMAT_MOD_LINEAR,
            plane: ImportedPlane {
                offset: 0,
                pitch: 256,
            },
            format: PackedImageFormat::Bgra8888,
            strategy: PackedImageImportStrategy::DirectSampledImage,
        };
        assert_ne!(
            base,
            PackedSourceTopology {
                object_size: 8_448,
                ..base
            }
        );
        assert_ne!(
            base,
            PackedSourceTopology {
                format: PackedImageFormat::Rgba8888,
                ..base
            }
        );
        assert_ne!(
            base,
            PackedSourceTopology {
                strategy: PackedImageImportStrategy::LinearBufferToOptimalBgra,
                ..base
            }
        );
    }

    #[test]
    fn packed_layout_requires_exact_pitch_and_truthful_object_span() {
        let plane = ImportedPlane {
            offset: 64,
            pitch: 256,
        };
        assert!(validate_packed_layout((64, 32), 8_256, plane).is_ok());
        assert!(
            validate_packed_layout((64, 32), 8_255, plane)
                .unwrap_err()
                .contains("smaller than packed image span")
        );
        assert!(
            validate_packed_layout(
                (64, 32),
                8_256,
                ImportedPlane {
                    offset: 64,
                    pitch: 255,
                },
            )
            .unwrap_err()
            .contains("pitch")
        );
    }

    #[test]
    fn maps_exact_bt709_color_and_siting() {
        let conversion = map_nv12_colorimetry(color(ChromaLocation::Left)).unwrap();
        assert_eq!(conversion.model, YcbcrModel::Bt709);
        assert_eq!(conversion.range, YcbcrRange::Narrow);
        assert_eq!(conversion.x_offset, YcbcrOffset::CositedEven);
        assert_eq!(conversion.y_offset, YcbcrOffset::Midpoint);
        assert!(map_nv12_colorimetry(Colorimetry::default()).is_err());
        assert!(map_nv12_colorimetry(color(ChromaLocation::Bottom)).is_err());
    }

    #[test]
    fn validates_shared_linear_nv12_topology_without_trusting_modifier_plane_inventory() {
        let layout = validate_nv12_shared_object_topology(
            (64, 32),
            &[3_072],
            &[Some(DRM_FORMAT_MOD_LINEAR)],
            &[
                Nv12Plane {
                    object_index: 0,
                    offset: 0,
                    pitch: 64,
                },
                Nv12Plane {
                    object_index: 0,
                    offset: 2_048,
                    pitch: 64,
                },
            ],
        )
        .unwrap();
        assert_eq!(layout.object_size, 3_072);
        assert_eq!(layout.planes[1].offset, 2_048);
    }

    #[test]
    fn staged_capability_accepts_v3dv_one_plane_inventory_without_calling_it_direct() {
        let capability = Nv12ModifierCapability {
            modifier: DRM_FORMAT_MOD_LINEAR,
            strategy: Nv12ImportStrategy::LinearBufferToYuvPlanes,
            modifier_plane_count: 1,
            source_tiling_features: vk::FormatFeatureFlags::TRANSFER_SRC,
            sampled_tiling_features: vk::FormatFeatureFlags::SAMPLED_IMAGE
                | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR
                | vk::FormatFeatureFlags::STORAGE_IMAGE
                | vk::FormatFeatureFlags::TRANSFER_SRC
                | vk::FormatFeatureFlags::TRANSFER_DST,
            external_features: vk::ExternalMemoryFeatureFlags::IMPORTABLE,
            compatible_handle_types: vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
            max_extent: vk::Extent3D {
                width: 4096,
                height: 4096,
                depth: 1,
            },
        };
        validate_nv12_modifier_capability(
            capability,
            (64, 32),
            map_nv12_colorimetry(color(ChromaLocation::Left)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            capability.allocation_recipe(),
            Nv12AllocationBindingRecipe::LinearBufferToYuvPlanes
        );
    }

    #[test]
    fn transfer_capabilities_require_their_exact_sampling_and_transfer_features() {
        let conversion = map_nv12_colorimetry(color(ChromaLocation::Left)).unwrap();
        let capability = Nv12ModifierCapability {
            modifier: DRM_FORMAT_MOD_LINEAR,
            strategy: Nv12ImportStrategy::LinearBufferToOptimalNv12,
            modifier_plane_count: 1,
            source_tiling_features: vk::FormatFeatureFlags::empty(),
            sampled_tiling_features: conversion.required_direct_features()
                | vk::FormatFeatureFlags::TRANSFER_SRC
                | vk::FormatFeatureFlags::TRANSFER_DST,
            external_features: vk::ExternalMemoryFeatureFlags::IMPORTABLE,
            compatible_handle_types: vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
            max_extent: vk::Extent3D {
                width: 4096,
                height: 4096,
                depth: 1,
            },
        };
        validate_nv12_modifier_capability(capability, (64, 32), conversion).unwrap();
        let separate = Nv12ModifierCapability {
            strategy: Nv12ImportStrategy::LinearBufferToOptimalYuvPlanes,
            sampled_tiling_features: vk::FormatFeatureFlags::SAMPLED_IMAGE
                | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR
                | vk::FormatFeatureFlags::TRANSFER_SRC
                | vk::FormatFeatureFlags::TRANSFER_DST,
            ..capability
        };
        validate_nv12_modifier_capability(separate, (64, 32), conversion).unwrap();
        assert!(
            validate_nv12_modifier_capability(
                separate,
                (64, 32),
                Nv12Conversion {
                    model: YcbcrModel::Bt601,
                    ..conversion
                },
            )
            .is_err()
        );
        assert!(
            validate_nv12_modifier_capability(
                Nv12ModifierCapability {
                    sampled_tiling_features: capability.sampled_tiling_features
                        & !vk::FormatFeatureFlags::TRANSFER_DST,
                    ..capability
                },
                (64, 32),
                conversion,
            )
            .is_err()
        );
    }

    #[test]
    fn staged_nv12_sources_declare_only_the_selected_read_operation() {
        assert!(STAGED_NV12_SOURCE_USAGE == vk::BufferUsageFlags::UNIFORM_TEXEL_BUFFER);
        assert!(STAGED_NV12_SOURCE_TEXEL_FORMAT == vk::Format::R32_UINT);
        assert!(!STAGED_NV12_SOURCE_USAGE.contains(vk::BufferUsageFlags::TRANSFER_SRC));
        assert!(TRANSFER_NV12_SOURCE_USAGE == vk::BufferUsageFlags::TRANSFER_SRC);
        assert!(!TRANSFER_NV12_SOURCE_USAGE.contains(vk::BufferUsageFlags::STORAGE_BUFFER));
    }

    #[test]
    fn transfer_nv12_outputs_require_no_storage_usage() {
        assert!(TRANSFER_NV12_OUTPUT_USAGE.contains(vk::ImageUsageFlags::SAMPLED));
        assert!(TRANSFER_NV12_OUTPUT_USAGE.contains(vk::ImageUsageFlags::TRANSFER_SRC));
        assert!(TRANSFER_NV12_OUTPUT_USAGE.contains(vk::ImageUsageFlags::TRANSFER_DST));
        assert!(!TRANSFER_NV12_OUTPUT_USAGE.contains(vk::ImageUsageFlags::STORAGE));
        let capability = |strategy| Nv12ModifierCapability {
            modifier: DRM_FORMAT_MOD_LINEAR,
            strategy,
            modifier_plane_count: 1,
            source_tiling_features: vk::FormatFeatureFlags::empty(),
            sampled_tiling_features: vk::FormatFeatureFlags::empty(),
            external_features: vk::ExternalMemoryFeatureFlags::empty(),
            compatible_handle_types: vk::ExternalMemoryHandleTypeFlags::empty(),
            max_extent: vk::Extent3D::default(),
        };
        assert_eq!(
            capability(Nv12ImportStrategy::LinearBufferToOptimalNv12).allocation_recipe(),
            Nv12AllocationBindingRecipe::LinearBufferToOptimalNv12
        );
        assert_eq!(
            capability(Nv12ImportStrategy::LinearBufferToOptimalYuvPlanes).allocation_recipe(),
            Nv12AllocationBindingRecipe::LinearBufferToOptimalYuvPlanes
        );
    }

    #[test]
    fn transfer_regions_preserve_exact_nv12_offsets_pitches_and_plane_extents() {
        let plan = StagedTransferPlan {
            source_buffer: vk::Buffer::null(),
            source_size: 3_840,
            output: StagedSampledImages::Nv12 {
                image: vk::Image::null(),
            },
            output_initialized: false,
            dimensions: (64, 32),
            planes: [
                ImportedPlane {
                    offset: 0,
                    pitch: 80,
                },
                ImportedPlane {
                    offset: 2_560,
                    pitch: 80,
                },
            ],
        };
        let regions = sync::nv12_multiplanar_transfer_regions(plan);
        assert_eq!(regions[0].buffer_offset, 0);
        assert_eq!(regions[0].buffer_row_length, 80);
        assert!(regions[0].image_subresource.aspect_mask == vk::ImageAspectFlags::PLANE_0);
        assert_eq!(regions[0].image_extent.width, 64);
        assert_eq!(regions[0].image_extent.height, 32);
        assert_eq!(regions[1].buffer_offset, 2_560);
        assert_eq!(regions[1].buffer_row_length, 40);
        assert!(regions[1].image_subresource.aspect_mask == vk::ImageAspectFlags::PLANE_1);
        assert_eq!(regions[1].image_extent.width, 32);
        assert_eq!(regions[1].image_extent.height, 16);

        let separate = sync::nv12_separate_transfer_regions(plan);
        assert!(
            separate
                .iter()
                .all(|region| region.image_subresource.aspect_mask == vk::ImageAspectFlags::COLOR)
        );
        assert_eq!(separate[0].buffer_row_length, 80);
        assert_eq!(separate[1].buffer_row_length, 40);
    }

    #[test]
    fn transfer_layout_fails_closed_on_unaligned_plane_offsets() {
        let layout = Nv12SharedObjectLayout {
            modifier: DRM_FORMAT_MOD_LINEAR,
            object_size: 3_840,
            planes: [
                ImportedPlane {
                    offset: 2,
                    pitch: 64,
                },
                ImportedPlane {
                    offset: 2_048,
                    pitch: 64,
                },
            ],
        };
        assert!(validate_transfer_layout((64, 32), layout).is_err());

        let valid = Nv12SharedObjectLayout {
            modifier: DRM_FORMAT_MOD_LINEAR,
            object_size: 3_840,
            planes: [
                ImportedPlane {
                    offset: 0,
                    pitch: 80,
                },
                ImportedPlane {
                    offset: 2_560,
                    pitch: 80,
                },
            ],
        };
        assert!(validate_transfer_layout((64, 32), valid).is_ok());
        assert!(
            validate_transfer_layout(
                (64, 32),
                Nv12SharedObjectLayout {
                    object_size: 3_800,
                    ..valid
                },
            )
            .is_err()
        );
        assert!(
            validate_transfer_layout(
                (64, 32),
                Nv12SharedObjectLayout {
                    planes: [
                        valid.planes[0],
                        ImportedPlane {
                            pitch: 79,
                            ..valid.planes[1]
                        },
                    ],
                    ..valid
                },
            )
            .is_err()
        );
        assert!(validate_transfer_layout((63, 32), valid).is_err());
    }

    #[test]
    fn staged_packed_source_uses_bounded_texel_fetch_and_mutable_bgra_output() {
        assert!(STAGED_PACKED_SOURCE_USAGE == vk::BufferUsageFlags::UNIFORM_TEXEL_BUFFER);
        assert!(STAGED_PACKED_SOURCE_TEXEL_FORMAT == vk::Format::R32_UINT);
        assert!(STAGED_PACKED_STORAGE_VIEW_FORMAT == vk::Format::R32_UINT);
        assert!(STAGED_PACKED_OUTPUT_USAGE.contains(vk::ImageUsageFlags::SAMPLED));
        assert!(STAGED_PACKED_OUTPUT_USAGE.contains(vk::ImageUsageFlags::STORAGE));
        assert!(!STAGED_PACKED_SOURCE_USAGE.contains(vk::BufferUsageFlags::TRANSFER_SRC));

        let bgra_word = |bytes: [u8; 4], format| match format {
            PackedImageFormat::Rgba8888 => {
                u32::from(bytes[2])
                    | (u32::from(bytes[1]) << 8)
                    | (u32::from(bytes[0]) << 16)
                    | (u32::from(bytes[3]) << 24)
            }
            PackedImageFormat::Bgra8888 => {
                u32::from(bytes[0])
                    | (u32::from(bytes[1]) << 8)
                    | (u32::from(bytes[2]) << 16)
                    | (255 << 24)
            }
        };
        assert_eq!(
            bgra_word([0x11, 0x22, 0x33, 0x44], PackedImageFormat::Rgba8888),
            0x4411_2233
        );
        assert_eq!(
            bgra_word([0x33, 0x22, 0x11, 0x00], PackedImageFormat::Bgra8888),
            0xff11_2233
        );
    }

    #[test]
    fn staged_outputs_declare_ganesh_transfer_compatibility_without_exposing_the_source() {
        assert!(STAGED_NV12_OUTPUT_USAGE.contains(vk::ImageUsageFlags::SAMPLED));
        assert!(STAGED_NV12_OUTPUT_USAGE.contains(vk::ImageUsageFlags::STORAGE));
        assert!(STAGED_NV12_OUTPUT_USAGE.contains(vk::ImageUsageFlags::TRANSFER_SRC));
        assert!(STAGED_NV12_OUTPUT_USAGE.contains(vk::ImageUsageFlags::TRANSFER_DST));
        assert!(!STAGED_NV12_SOURCE_USAGE.contains(vk::BufferUsageFlags::TRANSFER_SRC));
        assert!(!STAGED_NV12_SOURCE_USAGE.contains(vk::BufferUsageFlags::TRANSFER_DST));
    }

    #[test]
    fn embedded_compute_shaders_are_valid_spirv() {
        for output in [Nv12ComputeOutput::Rgba, Nv12ComputeOutput::YuvPlanes] {
            let words = shader_words(output).unwrap();
            assert_eq!(words.first().copied(), Some(0x0723_0203));
            assert!(words.len() > 16);
        }
        let packed = packed_shader_words().unwrap();
        assert_eq!(packed.first().copied(), Some(0x0723_0203));
        assert!(packed.len() > 16);
        assert!(std::mem::size_of::<Nv12PushConstants>() <= 128);
        assert!(std::mem::size_of::<PackedPushConstants>() <= 128);
    }

    #[test]
    fn default_import_pools_cover_camera_buffers_and_in_flight_outputs() {
        let limits = VulkanImportPoolLimits::default();
        assert!(limits.nv12_source_cache_entries >= 10);
        assert!(limits.nv12_output_slots >= 4);
        assert!(limits.packed_source_cache_entries >= 10);
        assert!(limits.packed_output_slots >= 4);
    }

    #[test]
    fn cached_source_active_reappearance_fails_closed_until_exact_release() {
        let claimed = AtomicBool::new(false);
        claim_idle_source(&claimed).unwrap();
        assert!(claim_idle_source(&claimed).is_err());
        assert!(claimed.swap(false, Ordering::AcqRel));
        claim_idle_source(&claimed).unwrap();
    }

    #[test]
    fn source_cache_identity_includes_stream_inode_and_exact_topology() {
        let topology = Nv12FrameTopology {
            dimensions: (64, 32),
            object_count: 1,
            object_size: 3_072,
            plane_count: 2,
            planes: [
                Nv12Plane {
                    object_index: 0,
                    offset: 0,
                    pitch: 64,
                },
                Nv12Plane {
                    object_index: 0,
                    offset: 2_048,
                    pitch: 64,
                },
            ],
            modifier: DRM_FORMAT_MOD_LINEAR,
        };
        let key = Nv12SourceCacheKey {
            stream_incarnation: 7,
            device: 3,
            inode: 11,
            topology,
            strategy: Nv12ImportStrategy::LinearBufferToYuvPlanes,
        };
        assert_ne!(
            key,
            Nv12SourceCacheKey {
                stream_incarnation: 8,
                ..key
            }
        );
        assert_ne!(key, Nv12SourceCacheKey { inode: 12, ..key });
        assert_ne!(
            key,
            Nv12SourceCacheKey {
                topology: Nv12FrameTopology {
                    object_size: 3_328,
                    ..topology
                },
                ..key
            }
        );
        assert_ne!(
            key,
            Nv12SourceCacheKey {
                strategy: Nv12ImportStrategy::LinearBufferToOptimalNv12,
                ..key
            }
        );
    }

    #[test]
    fn planar_chroma_coordinates_match_left_and_midpoint_definitions() {
        let reconstructed_coordinate = |pixel: u32, offset: YcbcrOffset| {
            let p = pixel as f32 + 0.5;
            let image_shader_coordinate = p * 0.5
                + match offset {
                    YcbcrOffset::CositedEven => 0.25,
                    YcbcrOffset::Midpoint => 0.0,
                };
            image_shader_coordinate - 0.5
        };
        assert_eq!(reconstructed_coordinate(0, YcbcrOffset::CositedEven), 0.0);
        assert_eq!(reconstructed_coordinate(1, YcbcrOffset::CositedEven), 0.5);
        assert_eq!(reconstructed_coordinate(0, YcbcrOffset::Midpoint), -0.25);
        assert_eq!(reconstructed_coordinate(1, YcbcrOffset::Midpoint), 0.25);
    }

    #[test]
    fn direct_capability_still_requires_truthful_two_plane_linear_filter_support() {
        let conversion = map_nv12_colorimetry(color(ChromaLocation::Left)).unwrap();
        let capability = Nv12ModifierCapability {
            modifier: 7,
            strategy: Nv12ImportStrategy::DirectSampledImage,
            modifier_plane_count: 1,
            source_tiling_features: conversion.required_direct_features(),
            sampled_tiling_features: conversion.required_direct_features(),
            external_features: vk::ExternalMemoryFeatureFlags::IMPORTABLE,
            compatible_handle_types: vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
            max_extent: vk::Extent3D {
                width: 4096,
                height: 4096,
                depth: 1,
            },
        };
        assert!(validate_nv12_modifier_capability(capability, (64, 32), conversion).is_err());
    }
}
