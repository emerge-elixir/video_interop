use super::*;

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
    let mut capabilities = if staging_preference == Nv12StagingPreference::PreferPlanar {
        modifiers
            .iter()
            .filter_map(|modifier| {
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
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    if let Some(linear) = modifiers
        .iter()
        .find(|modifier| modifier.drm_format_modifier == DRM_FORMAT_MOD_LINEAR)
    {
        capabilities.extend(query_linear_nv12_staging_candidates(
            device,
            *linear,
            staging_preference,
        ));
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

fn query_linear_nv12_staging_candidates<D: VulkanDeviceContext>(
    device: &D,
    linear: vk::DrmFormatModifierPropertiesEXT,
    staging_preference: Nv12StagingPreference,
) -> Vec<Nv12ModifierCapability> {
    match staging_preference {
        Nv12StagingPreference::PreferPlanar => {
            match query_linear_nv12_transfer_capability(device, linear) {
                Ok(transfer) => vec![transfer],
                Err(_transfer_error) => query_linear_nv12_compute_capability(
                    device,
                    linear,
                    Nv12StagingPreference::PreferPlanar,
                )
                .into_iter()
                .collect(),
            }
        }
        Nv12StagingPreference::RequirePlanar | Nv12StagingPreference::RequireRgba => {
            query_linear_nv12_compute_capability(device, linear, staging_preference)
                .into_iter()
                .collect()
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
