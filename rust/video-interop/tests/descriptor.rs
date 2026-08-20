use video_interop::{
    Descriptor, FrameDescriptor, Layer, Modifier, Object, OwnedFrame, Plane, Rect, Storage,
    ValidationError,
};

fn assert_send<T: Send>() {}

fn nv12_descriptor() -> Descriptor {
    Descriptor {
        version: 1,
        objects: vec![Object {
            fd: 3,
            size: 5_529_600,
            modifier: Modifier::linear(),
        }],
        layers: vec![Layer {
            fourcc: u32::from_le_bytes(*b"NV12"),
            planes: vec![
                Plane {
                    object_index: 0,
                    offset: 0,
                    pitch: 2560,
                },
                Plane {
                    object_index: 0,
                    offset: 3_686_400,
                    pitch: 2560,
                },
            ],
        }],
    }
}

#[test]
fn owned_frames_can_move_to_native_retirement_threads() {
    assert_send::<OwnedFrame>();
}

#[test]
fn accepts_one_object_nv12() {
    assert_eq!(nv12_descriptor().validate(), Ok(()));
}

#[test]
fn accepts_explicit_and_implicit_modifiers_as_distinct_values() {
    assert_eq!(Modifier::linear().explicit(), Some(0));
    assert_eq!(Modifier::Implicit.explicit(), None);
}

#[test]
fn rejects_oversized_avdrm_descriptors() {
    let mut descriptor = nv12_descriptor();
    descriptor.objects = (0..5)
        .map(|fd| Object {
            fd,
            size: 4096,
            modifier: Modifier::Implicit,
        })
        .collect();

    assert_eq!(
        descriptor.validate(),
        Err(ValidationError::TooManyEntries {
            kind: "objects",
            actual: 5,
            maximum: 4,
        })
    );
}

#[test]
fn rejects_unreferenced_objects() {
    let mut descriptor = nv12_descriptor();
    descriptor.objects.push(Object {
        fd: 4,
        size: 4_096,
        modifier: Modifier::linear(),
    });

    assert_eq!(
        descriptor.validate(),
        Err(ValidationError::UnreferencedObject { index: 1 })
    );
}

#[test]
fn rejects_invalid_object_index() {
    let mut descriptor = nv12_descriptor();
    descriptor.layers[0].planes[1].object_index = 1;

    assert_eq!(
        descriptor.validate(),
        Err(ValidationError::InvalidObjectIndex {
            layer: 0,
            plane: 1,
            object_index: 1,
            object_count: 1,
        })
    );
}

#[test]
fn rejects_plane_offset_outside_object() {
    let mut descriptor = nv12_descriptor();
    descriptor.layers[0].planes[1].offset = descriptor.objects[0].size;

    assert_eq!(
        descriptor.validate(),
        Err(ValidationError::PlaneOffsetOutOfBounds {
            layer: 0,
            plane: 1,
            object_index: 0,
            offset: 5_529_600,
            object_size: 5_529_600,
        })
    );
}

#[test]
fn validates_visible_geometry_before_fd_duplication() {
    let frame = FrameDescriptor {
        coded_width: 640,
        coded_height: 480,
        visible_rect: Rect {
            x: 1,
            y: 0,
            width: 640,
            height: 480,
        },
        storage: Storage::DmaBuf(nv12_descriptor()),
        acquire_sync: video_interop::AcquireSync::Implicit,
    };

    assert!(matches!(
        frame.validate(),
        Err(ValidationError::VisibleRectOutOfBounds { .. })
    ));
}
