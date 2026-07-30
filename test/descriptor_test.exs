defmodule VideoInterop.DescriptorTest do
  use ExUnit.Case, async: true

  alias VideoInterop.DMABuf.{Descriptor, FourCC, Layer, Modifier, Object, Plane}

  test "validates one-object NV12 and distinguishes implicit from linear modifiers" do
    descriptor = nv12_descriptor(Modifier.linear())

    assert :ok = VideoInterop.validate(descriptor)
    assert descriptor.objects |> hd() |> Map.fetch!(:modifier) == 0
    assert :ok = VideoInterop.validate(nv12_descriptor(:implicit))
  end

  test "supports two-object NV12" do
    descriptor = %Descriptor{
      objects: [
        %Object{fd: 10, size: 3_686_400, modifier: 0},
        %Object{fd: 11, size: 1_843_200, modifier: 0}
      ],
      layers: [
        %Layer{
          fourcc: FourCC.nv12(),
          planes: [
            %Plane{object_index: 0, offset: 0, pitch: 2560},
            %Plane{object_index: 1, offset: 0, pitch: 2560}
          ]
        }
      ]
    }

    assert :ok = VideoInterop.validate(descriptor)
  end

  test "supports layouts without a fixed format registry" do
    i420 =
      descriptor(
        "YU12",
        [
          %Plane{object_index: 0, offset: 0, pitch: 640},
          %Plane{object_index: 0, offset: 307_200, pitch: 320},
          %Plane{object_index: 0, offset: 384_000, pitch: 320}
        ],
        460_800
      )

    yuyv = descriptor("YUYV", [%Plane{object_index: 0, offset: 0, pitch: 1280}], 614_400)
    xrgb = descriptor("XR24", [%Plane{object_index: 0, offset: 0, pitch: 2560}], 1_228_800)

    for value <- [i420, yuyv, xrgb], do: assert(:ok = VideoInterop.validate(value))
  end

  test "rejects improper and oversized AVDRM lists without raising" do
    descriptor = nv12_descriptor(0)
    [object] = descriptor.objects

    assert {:error, {:invalid_field, [:descriptor, :objects], _value}} =
             VideoInterop.validate(%{descriptor | objects: [object | :improper]})

    assert {:error, {:invalid_field, [:descriptor, :objects], _value}} =
             VideoInterop.validate(%{descriptor | objects: List.duplicate(object, 5)})

    [layer] = descriptor.layers
    [plane | _rest] = layer.planes
    layers = [%{layer | planes: List.duplicate(plane, 3)}, %{layer | planes: [plane, plane]}]

    assert {:error, {:too_many_planes, 5, 4}} =
             VideoInterop.validate(%{descriptor | layers: layers})
  end

  test "rejects unsupported versions, references, and offsets" do
    descriptor = nv12_descriptor(0)

    assert {:error, {:unsupported_descriptor_version, 2}} =
             VideoInterop.validate(%{descriptor | version: 2})

    [layer] = descriptor.layers
    [y, uv] = layer.planes

    assert {:error, {:invalid_object_index, 0, 1, 1}} =
             VideoInterop.validate(%{
               descriptor
               | layers: [%{layer | planes: [y, %{uv | object_index: 1}]}]
             })

    object_size = descriptor.objects |> hd() |> Map.fetch!(:size)

    assert {:error, {:plane_offset_out_of_bounds, 0, 1, ^object_size}} =
             VideoInterop.validate(%{
               descriptor
               | layers: [%{layer | planes: [y, %{uv | offset: object_size}]}]
             })
  end

  defp nv12_descriptor(modifier) do
    descriptor(
      "NV12",
      [
        %Plane{object_index: 0, offset: 0, pitch: 2560},
        %Plane{object_index: 0, offset: 3_686_400, pitch: 2560}
      ],
      5_529_600,
      modifier
    )
  end

  defp descriptor(fourcc, planes, size, modifier \\ :implicit) do
    %Descriptor{
      objects: [%Object{fd: 10, size: size, modifier: modifier}],
      layers: [%Layer{fourcc: FourCC.from_string!(fourcc), planes: planes}]
    }
  end
end
