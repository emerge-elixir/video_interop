
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
            .to_string()
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
        .to_string()
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
        source_size: 3_824,
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
    assert_eq!(nv12_transfer_source_span((64, 32), valid).unwrap(), 3_824);

    let camera = Nv12SharedObjectLayout {
        modifier: DRM_FORMAT_MOD_LINEAR,
        object_size: 5_529_856,
        planes: [
            ImportedPlane {
                offset: 0,
                pitch: 2_560,
            },
            ImportedPlane {
                offset: 3_686_400,
                pitch: 2_560,
            },
        ],
    };
    assert_eq!(
        nv12_transfer_source_span((2_560, 1_440), camera).unwrap(),
        5_529_600
    );
    assert_eq!(camera.object_size - 5_529_600, 256);
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
fn compute_nv12_source_span_excludes_allocation_tail_and_bounds_u32_addressing() {
    let camera = Nv12SharedObjectLayout {
        modifier: DRM_FORMAT_MOD_LINEAR,
        object_size: 5_529_856,
        planes: [
            ImportedPlane {
                offset: 0,
                pitch: 2_560,
            },
            ImportedPlane {
                offset: 3_686_400,
                pitch: 2_560,
            },
        ],
    };
    assert_eq!(
        nv12_compute_source_span((2_560, 1_440), camera).unwrap(),
        5_529_600
    );

    let overflow = Nv12SharedObjectLayout {
        modifier: DRM_FORMAT_MOD_LINEAR,
        object_size: u64::from(u32::MAX) + 4_097,
        planes: [
            ImportedPlane {
                offset: u64::from(u32::MAX) - 31,
                pitch: 64,
            },
            ImportedPlane {
                offset: 0,
                pitch: 64,
            },
        ],
    };
    assert!(nv12_compute_source_span((64, 32), overflow).unwrap() > u64::from(u32::MAX) + 1);
}

#[cfg(target_os = "linux")]
#[test]
fn dmabuf_identity_requires_exact_fd_backed_size_and_restores_position() {
    let name = c"video-interop-size-test";
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    assert!(fd >= 0);
    assert_eq!(unsafe { libc::ftruncate(fd, 4_096) }, 0);
    assert_eq!(unsafe { libc::lseek(fd, 17, libc::SEEK_SET) }, 17);

    let identity = verified_dmabuf_identity(fd, 4_096).unwrap();
    assert_eq!(identity.allocation_size, 4_096);
    assert_eq!(unsafe { libc::lseek(fd, 0, libc::SEEK_CUR) }, 17);
    assert!(verified_dmabuf_identity(fd, 4_095).is_err());
    assert!(verified_dmabuf_identity(fd, 4_097).is_err());
    assert_eq!(unsafe { libc::close(fd) }, 0);
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
fn resolver_uses_a_staged_candidate_when_direct_sampling_fails_exact_conversion() {
    let conversion = map_nv12_colorimetry(color(ChromaLocation::Left)).unwrap();
    let direct = Nv12ModifierCapability {
        modifier: DRM_FORMAT_MOD_LINEAR,
        strategy: Nv12ImportStrategy::DirectSampledImage,
        modifier_plane_count: 2,
        source_tiling_features: vk::FormatFeatureFlags::SAMPLED_IMAGE,
        sampled_tiling_features: vk::FormatFeatureFlags::SAMPLED_IMAGE,
        external_features: vk::ExternalMemoryFeatureFlags::IMPORTABLE,
        compatible_handle_types: vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
        max_extent: vk::Extent3D {
            width: 4_096,
            height: 4_096,
            depth: 1,
        },
    };
    let transfer = Nv12ModifierCapability {
        strategy: Nv12ImportStrategy::LinearBufferToOptimalYuvPlanes,
        modifier_plane_count: 1,
        sampled_tiling_features: vk::FormatFeatureFlags::SAMPLED_IMAGE
            | vk::FormatFeatureFlags::SAMPLED_IMAGE_FILTER_LINEAR
            | vk::FormatFeatureFlags::TRANSFER_SRC
            | vk::FormatFeatureFlags::TRANSFER_DST,
        ..direct
    };
    let selected = resolve_nv12_modifier_capability(
        &[direct, transfer],
        Nv12ResolveRequest {
            modifier: DRM_FORMAT_MOD_LINEAR,
            dimensions: (2_560, 1_440),
            conversion,
        },
    )
    .unwrap();
    assert_eq!(
        selected.strategy,
        Nv12ImportStrategy::LinearBufferToOptimalYuvPlanes
    );
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
