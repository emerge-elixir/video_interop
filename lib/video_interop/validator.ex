defmodule VideoInterop.Validator do
  @moduledoc "Structural validation and producer-authority verification for borrowed frames."

  alias VideoInterop.{AbandonmentGuard, Binary, Colorimetry, Frame, Lease, Rect, SyncFile}
  alias VideoInterop.Binary.Plane, as: BinaryPlane

  alias VideoInterop.DMABuf.{
    Descriptor,
    Layer,
    Modifier,
    Object,
    Plane
  }

  alias VideoInterop.Format, as: VideoFormat
  alias VideoInterop.DMABuf.Format, as: DMABufFormat

  @max_i32 0x7FFF_FFFF
  @max_u32 0xFFFF_FFFF
  @max_u64 0xFFFF_FFFF_FFFF_FFFF
  @max_avdrm_entries 4

  @primaries ~w(unspecified bt709 bt470_m bt470_bg smpte170m smpte240m film bt2020 smpte428 smpte431 smpte432 ebu3213)a
  @transfers ~w(unspecified bt709 gamma22 gamma28 smpte170m smpte240m linear log log_sqrt iec61966_2_4 bt1361 iec61966_2_1 bt2020_10 bt2020_12 smpte2084 smpte428 arib_std_b67)a
  @matrices ~w(unspecified rgb bt709 fcc bt470_bg smpte170m smpte240m ycgco bt2020_ncl bt2020_cl smpte2085 chroma_derived_ncl chroma_derived_cl ictcp)a
  @ranges ~w(unspecified limited full)a
  @chroma_locations ~w(unspecified left center top_left top bottom_left bottom)a
  @interlace_modes ~w(progressive interlaced_top_first interlaced_bottom_first mixed)a

  @type reason :: term()

  @spec validate_descriptor(term()) :: :ok | {:error, reason()}
  def validate_descriptor(%Descriptor{version: version, objects: objects, layers: layers}) do
    with :ok <- descriptor_version(version),
         :ok <- bounded_nonempty_list(objects, @max_avdrm_entries, [:descriptor, :objects]),
         :ok <- bounded_nonempty_list(layers, @max_avdrm_entries, [:descriptor, :layers]),
         :ok <- validate_objects(objects),
         :ok <- validate_layers(layers, objects),
         :ok <- validate_total_plane_count(layers),
         :ok <- validate_referenced_objects(layers, objects) do
      :ok
    end
  end

  def validate_descriptor(value), do: {:error, {:invalid_descriptor, value}}

  @spec validate_frame(term()) :: :ok | {:error, reason()}
  def validate_frame(%Frame{} = frame) do
    with :ok <- positive_bounded(frame.coded_width, @max_u32, [:frame, :coded_width]),
         :ok <- positive_bounded(frame.coded_height, @max_u32, [:frame, :coded_height]),
         :ok <- validate_rect(frame.visible_rect, frame.coded_width, frame.coded_height),
         :ok <- validate_storage(frame.storage, frame),
         :ok <- validate_acquire_sync(frame.acquire_sync),
         :ok <- validate_frame_format(frame),
         :ok <- validate_storage_ownership(frame) do
      :ok
    end
  end

  def validate_frame(value), do: {:error, {:invalid_frame, value}}

  @spec validate_format(term()) :: :ok | {:error, reason()}
  def validate_format(%VideoFormat{} = format) do
    with :ok <- positive_bounded(format.width, @max_u32, [:format, :width]),
         :ok <- positive_bounded(format.height, @max_u32, [:format, :height]),
         :ok <- optional_rational(format.framerate, [:format, :framerate]),
         :ok <- validate_storage_format(format.storage),
         :ok <- stream_acquire_sync(format.acquire_sync),
         :ok <- storage_acquire_sync(format.storage, format.acquire_sync),
         :ok <- colorimetry(format.colorimetry),
         :ok <- rational(format.pixel_aspect_ratio, [:format, :pixel_aspect_ratio]),
         :ok <- interlace_mode(format.interlace_mode),
         :ok <- alpha_mode(format.alpha_mode),
         :ok <- storage_alpha_mode(format.storage, format.alpha_mode) do
      :ok
    end
  end

  def validate_format(value), do: {:error, {:invalid_format, value}}

  @spec validate_frame_against_format(term(), term()) :: :ok | {:error, reason()}
  def validate_frame_against_format(%Frame{} = frame, %VideoFormat{} = format) do
    with :ok <- validate_frame(frame),
         :ok <- validate_format(format),
         :ok <- matching_dimensions(frame, format),
         :ok <- matching_storage(frame, format),
         :ok <- matching_acquire_sync(frame.acquire_sync, format.acquire_sync) do
      :ok
    end
  end

  def validate_frame_against_format(frame, format),
    do: {:error, {:invalid_frame_format_pair, frame, format}}

  defp validate_frame_against_format_without_frame_validation(frame, format) do
    with :ok <- validate_format(format),
         :ok <- matching_dimensions(frame, format),
         :ok <- matching_storage(frame, format),
         :ok <- matching_acquire_sync(frame.acquire_sync, format.acquire_sync) do
      :ok
    end
  end

  defp validate_storage(%Descriptor{} = descriptor, _frame), do: validate_descriptor(descriptor)

  defp validate_storage(%Binary{} = storage, frame),
    do: validate_binary_storage(storage, frame)

  defp validate_storage(value, _frame), do: {:error, {:invalid_storage, value}}

  defp validate_storage_format(%DMABufFormat{} = format) do
    with :ok <- positive_bounded(format.fourcc, @max_u32, [:format, :storage, :fourcc]),
         :ok <- stream_modifier(format.modifier) do
      :ok
    end
  end

  defp validate_storage_format(%Binary.Format{} = format) do
    binary_format(format)
  end

  defp validate_storage_format(value), do: {:error, {:invalid_storage_format, value}}

  defp validate_objects(objects) do
    objects
    |> Enum.with_index()
    |> Enum.reduce_while(:ok, fn
      {%Object{} = object, index}, :ok ->
        result =
          with :ok <- unsigned_bounded(object.fd, @max_i32, [:objects, index, :fd]),
               :ok <- positive_bounded(object.size, @max_u64, [:objects, index, :size]),
               true <- Modifier.valid?(object.modifier) do
            :ok
          else
            false -> {:error, {:invalid_modifier, index, object.modifier}}
            {:error, _reason} = error -> error
          end

        reduce_result(result)

      {object, index}, :ok ->
        {:halt, {:error, {:invalid_object, index, object}}}
    end)
  end

  defp validate_layers(layers, objects) do
    layers
    |> Enum.with_index()
    |> Enum.reduce_while(:ok, fn
      {%Layer{} = layer, layer_index}, :ok ->
        result =
          with :ok <- positive_bounded(layer.fourcc, @max_u32, [:layers, layer_index, :fourcc]),
               :ok <-
                 bounded_nonempty_list(
                   layer.planes,
                   @max_avdrm_entries,
                   [:layers, layer_index, :planes]
                 ),
               :ok <- validate_planes(layer.planes, layer_index, objects) do
            :ok
          end

        reduce_result(result)

      {layer, index}, :ok ->
        {:halt, {:error, {:invalid_layer, index, layer}}}
    end)
  end

  defp validate_planes(planes, layer_index, objects) do
    planes
    |> Enum.with_index()
    |> Enum.reduce_while(:ok, fn
      {%Plane{} = plane, plane_index}, :ok ->
        reduce_result(validate_plane(plane, layer_index, plane_index, objects))

      {plane, plane_index}, :ok ->
        {:halt, {:error, {:invalid_plane, layer_index, plane_index, plane}}}
    end)
  end

  defp validate_plane(plane, layer_index, plane_index, objects) do
    with :ok <-
           unsigned_bounded(
             plane.object_index,
             @max_u32,
             [:layers, layer_index, :planes, plane_index, :object_index]
           ),
         {:ok, object} <- fetch_object(objects, layer_index, plane_index, plane.object_index),
         :ok <-
           unsigned_bounded(
             plane.offset,
             @max_u64,
             [:layers, layer_index, :planes, plane_index, :offset]
           ),
         :ok <-
           positive_bounded(
             plane.pitch,
             @max_u32,
             [:layers, layer_index, :planes, plane_index, :pitch]
           ),
         true <- plane.offset < object.size do
      :ok
    else
      false -> {:error, {:plane_offset_out_of_bounds, layer_index, plane_index, plane.offset}}
      {:error, _reason} = error -> error
    end
  end

  defp fetch_object(objects, layer_index, plane_index, object_index) do
    case Enum.fetch(objects, object_index) do
      {:ok, object} -> {:ok, object}
      :error -> {:error, {:invalid_object_index, layer_index, plane_index, object_index}}
    end
  end

  defp validate_rect(%Rect{} = rect, coded_width, coded_height) do
    with :ok <- unsigned_bounded(rect.x, @max_u32, [:frame, :visible_rect, :x]),
         :ok <- unsigned_bounded(rect.y, @max_u32, [:frame, :visible_rect, :y]),
         :ok <- positive_bounded(rect.width, @max_u32, [:frame, :visible_rect, :width]),
         :ok <- positive_bounded(rect.height, @max_u32, [:frame, :visible_rect, :height]),
         true <- rect.x + rect.width <= coded_width,
         true <- rect.y + rect.height <= coded_height do
      :ok
    else
      false -> {:error, {:visible_rect_out_of_bounds, rect, {coded_width, coded_height}}}
      {:error, _reason} = error -> error
    end
  end

  defp validate_rect(value, _coded_width, _coded_height),
    do: {:error, {:invalid_visible_rect, value}}

  defp validate_acquire_sync(:implicit), do: :ok

  defp validate_acquire_sync(%SyncFile{acquire_fence_fd: fd}),
    do: unsigned_bounded(fd, @max_i32, [:frame, :acquire_sync, :acquire_fence_fd])

  defp validate_acquire_sync(value), do: {:error, {:invalid_acquire_sync, value}}

  defp stream_acquire_sync(policy) when policy in [:implicit, :sync_file, :per_frame], do: :ok

  defp stream_acquire_sync(policy),
    do: {:error, {:invalid_field, [:format, :acquire_sync], policy}}

  defp storage_acquire_sync(%Binary.Format{}, :implicit), do: :ok

  defp storage_acquire_sync(%Binary.Format{}, policy),
    do: {:error, {:binary_format_requires_implicit_sync, policy}}

  defp storage_acquire_sync(%DMABufFormat{}, _policy), do: :ok

  defp storage_alpha_mode(%Binary.Format{pixel_format: :rgba8888}, _alpha_mode), do: :ok
  defp storage_alpha_mode(%Binary.Format{}, :opaque), do: :ok

  defp storage_alpha_mode(%Binary.Format{}, alpha_mode),
    do: {:error, {:binary_format_requires_opaque_alpha, alpha_mode}}

  defp storage_alpha_mode(%DMABufFormat{}, _alpha_mode), do: :ok

  defp matching_acquire_sync(_actual, :per_frame), do: :ok
  defp matching_acquire_sync(:implicit, :implicit), do: :ok
  defp matching_acquire_sync(%SyncFile{}, :sync_file), do: :ok

  defp matching_acquire_sync(actual, expected),
    do: {:error, {:acquire_sync_mismatch, acquire_sync_kind(actual), expected}}

  defp acquire_sync_kind(:implicit), do: :implicit
  defp acquire_sync_kind(%SyncFile{}), do: :sync_file

  defp validate_lease(%Lease{owner: owner, holder: holder, abandonment_guard: guard} = lease)
       when is_pid(owner) and is_reference(holder) do
    cond do
      node(owner) != node() -> {:error, {:remote_lease_owner, owner}}
      is_nil(guard) -> :ok
      AbandonmentGuard.valid?(guard) -> :ok
      true -> {:error, {:invalid_lease, lease}}
    end
  end

  defp validate_lease(value), do: {:error, {:invalid_lease, value}}

  defp validate_storage_ownership(%Frame{
         storage: %Binary{},
         lease: nil,
         acquire_sync: :implicit
       }),
       do: :ok

  defp validate_storage_ownership(%Frame{storage: %Binary{}, lease: lease})
       when not is_nil(lease),
       do: {:error, {:binary_storage_must_not_have_lease, lease}}

  defp validate_storage_ownership(%Frame{storage: %Binary{}, acquire_sync: sync}),
    do: {:error, {:binary_storage_requires_implicit_sync, sync}}

  defp validate_storage_ownership(%Frame{storage: %Descriptor{}, lease: lease}),
    do: validate_lease(lease)

  defp validate_frame_format(%Frame{format: nil, storage: %Descriptor{}}), do: :ok

  defp validate_frame_format(%Frame{format: %VideoFormat{} = format} = frame),
    do: validate_frame_against_format_without_frame_validation(frame, format)

  defp validate_frame_format(%Frame{format: value}), do: {:error, {:invalid_frame_format, value}}

  defp matching_dimensions(frame, format) do
    if frame.coded_width == format.width and frame.coded_height == format.height do
      :ok
    else
      {:error,
       {:coded_size_mismatch, {frame.coded_width, frame.coded_height},
        {format.width, format.height}}}
    end
  end

  defp matching_storage(
         %Frame{storage: %Descriptor{layers: [primary | _]} = descriptor},
         %VideoFormat{storage: %DMABufFormat{} = format}
       ) do
    with :ok <- matching_fourcc(primary.fourcc, format.fourcc),
         :ok <- matching_modifier(descriptor, format.modifier) do
      :ok
    end
  end

  defp matching_storage(
         %Frame{
           storage: %Binary{},
           format: %VideoFormat{storage: %Binary.Format{} = actual}
         },
         %VideoFormat{storage: %Binary.Format{} = expected}
       ) do
    if actual == expected,
      do: :ok,
      else: {:error, {:binary_format_mismatch, actual, expected}}
  end

  defp matching_storage(frame, format),
    do: {:error, {:storage_format_mismatch, frame.storage, format.storage}}

  defp matching_fourcc(fourcc, fourcc), do: :ok
  defp matching_fourcc(actual, expected), do: {:error, {:fourcc_mismatch, actual, expected}}

  defp matching_modifier(_descriptor, :per_buffer), do: :ok

  defp matching_modifier(descriptor, modifier) do
    case Enum.find(descriptor.objects, &(&1.modifier != modifier)) do
      nil -> :ok
      object -> {:error, {:modifier_mismatch, object.modifier, modifier}}
    end
  end

  defp validate_binary_storage(
         %Binary{data: data, planes: [%BinaryPlane{offset: offset, stride: stride}]},
         %Frame{coded_width: width, coded_height: height, format: %VideoFormat{storage: format}}
       )
       when is_binary(data) do
    with :ok <- binary_format(format),
         :ok <- unsigned_bounded(offset, @max_u64, [:frame, :storage, :planes, 0, :offset]),
         :ok <- positive_bounded(stride, @max_u32, [:frame, :storage, :planes, 0, :stride]),
         {:ok, minimum_stride} <- binary_minimum_stride(width, format.pixel_format),
         true <- stride >= minimum_stride,
         {:ok, required_bytes} <-
           binary_required_bytes(offset, stride, height, minimum_stride),
         true <- byte_size(data) >= required_bytes do
      :ok
    else
      false -> {:error, {:binary_storage_too_small, byte_size(data), width, height, stride}}
      {:error, _reason} = error -> error
    end
  end

  defp validate_binary_storage(%Binary{planes: planes}, _frame),
    do: {:error, {:unsupported_binary_planes, planes}}

  defp binary_format(%Binary.Format{pixel_format: pixel_format, bw1_polarity: polarity})
       when pixel_format in [:rgba8888, :rgb888, :gray8, :gray2] and is_nil(polarity),
       do: :ok

  defp binary_format(%Binary.Format{pixel_format: :bw1, bw1_polarity: polarity})
       when polarity in [:one_is_black, :one_is_white],
       do: :ok

  defp binary_format(format), do: {:error, {:invalid_binary_format, format}}

  defp binary_minimum_stride(width, :rgba8888), do: checked_stride(width, 4)
  defp binary_minimum_stride(width, :rgb888), do: checked_stride(width, 3)
  defp binary_minimum_stride(width, :gray8), do: checked_stride(width, 1)
  defp binary_minimum_stride(width, :gray2), do: {:ok, div(width + 3, 4)}
  defp binary_minimum_stride(width, :bw1), do: {:ok, div(width + 7, 8)}

  defp checked_stride(width, bytes_per_pixel) do
    case width * bytes_per_pixel do
      stride when stride <= @max_u32 -> {:ok, stride}
      _stride -> {:error, {:binary_stride_overflow, width, bytes_per_pixel}}
    end
  end

  defp binary_required_bytes(offset, stride, height, row_bytes) do
    required = offset + stride * (height - 1) + row_bytes

    if required <= @max_u64,
      do: {:ok, required},
      else: {:error, {:binary_size_overflow, offset, stride, height}}
  end

  defp descriptor_version(1), do: :ok
  defp descriptor_version(version), do: {:error, {:unsupported_descriptor_version, version}}

  defp stream_modifier(:per_buffer), do: :ok

  defp stream_modifier(modifier) do
    if Modifier.valid?(modifier),
      do: :ok,
      else: {:error, {:invalid_stream_modifier, modifier}}
  end

  defp colorimetry(%Colorimetry{} = colorimetry) do
    with :ok <- member(colorimetry.primaries, @primaries, [:format, :colorimetry, :primaries]),
         :ok <- member(colorimetry.transfer, @transfers, [:format, :colorimetry, :transfer]),
         :ok <- member(colorimetry.matrix, @matrices, [:format, :colorimetry, :matrix]),
         :ok <- member(colorimetry.range, @ranges, [:format, :colorimetry, :range]),
         :ok <-
           member(
             colorimetry.chroma_location,
             @chroma_locations,
             [:format, :colorimetry, :chroma_location]
           ) do
      :ok
    end
  end

  defp colorimetry(value), do: {:error, {:invalid_colorimetry, value}}

  defp interlace_mode(mode), do: member(mode, @interlace_modes, [:format, :interlace_mode])

  defp alpha_mode(mode) when mode in [:opaque, :straight, :premultiplied], do: :ok
  defp alpha_mode(mode), do: {:error, {:invalid_alpha_mode, mode}}

  defp optional_rational(nil, _path), do: :ok
  defp optional_rational(value, path), do: rational(value, path)

  defp rational({numerator, denominator}, path) do
    with :ok <- positive_bounded(numerator, @max_u32, path ++ [:numerator]),
         :ok <- positive_bounded(denominator, @max_u32, path ++ [:denominator]) do
      :ok
    end
  end

  defp rational(value, path), do: {:error, {:invalid_field, path, value}}

  defp bounded_nonempty_list(value, max, _path)
       when is_list(value) and value != [] and length(value) <= max,
       do: :ok

  defp bounded_nonempty_list(value, _max, path),
    do: {:error, {:invalid_field, path, value}}

  defp validate_referenced_objects(layers, objects) do
    referenced =
      layers
      |> Enum.flat_map(& &1.planes)
      |> Enum.map(& &1.object_index)
      |> MapSet.new()

    case objects
         |> Enum.with_index()
         |> Enum.find(fn {_object, index} -> not MapSet.member?(referenced, index) end) do
      nil -> :ok
      {_object, index} -> {:error, {:unreferenced_object, index}}
    end
  end

  defp validate_total_plane_count(layers) do
    count = Enum.reduce(layers, 0, fn layer, total -> total + length(layer.planes) end)

    if count <= @max_avdrm_entries,
      do: :ok,
      else: {:error, {:too_many_planes, count, @max_avdrm_entries}}
  end

  defp member(value, allowed, path) do
    if value in allowed, do: :ok, else: {:error, {:invalid_field, path, value}}
  end

  defp positive_bounded(value, max, _path)
       when is_integer(value) and value > 0 and value <= max,
       do: :ok

  defp positive_bounded(value, _max, path), do: {:error, {:invalid_field, path, value}}

  defp unsigned_bounded(value, max, _path)
       when is_integer(value) and value >= 0 and value <= max,
       do: :ok

  defp unsigned_bounded(value, _max, path), do: {:error, {:invalid_field, path, value}}

  defp reduce_result(:ok), do: {:cont, :ok}
  defp reduce_result({:error, _reason} = error), do: {:halt, error}
end
