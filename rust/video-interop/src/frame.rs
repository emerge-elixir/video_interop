use crate::{
    AcquireSync, Descriptor, DuplicateError, OwnedAcquireSync, OwnedDescriptor, Rect,
    ValidationError,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "rustler", derive(rustler::NifStruct))]
#[cfg_attr(feature = "rustler", module = "VideoInterop.Binary.Plane")]
pub struct BinaryPlane {
    pub offset: u64,
    pub stride: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BinaryStorage {
    pub data: Vec<u8>,
    pub planes: Vec<BinaryPlane>,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Storage {
    Binary(BinaryStorage),
    DmaBuf(Descriptor),
}

#[derive(Debug)]
#[non_exhaustive]
pub enum OwnedStorage {
    Binary(BinaryStorage),
    DmaBuf(OwnedDescriptor),
}

#[derive(Clone, Debug)]
pub struct FrameDescriptor {
    pub coded_width: u32,
    pub coded_height: u32,
    pub visible_rect: Rect,
    pub storage: Storage,
    pub acquire_sync: AcquireSync,
}

#[derive(Debug)]
pub struct OwnedFrame {
    pub coded_width: u32,
    pub coded_height: u32,
    pub visible_rect: Rect,
    pub storage: OwnedStorage,
    pub acquire_sync: OwnedAcquireSync,
}

impl Storage {
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Binary(storage) => storage.validate(),
            Self::DmaBuf(descriptor) => descriptor.validate(),
        }
    }

    pub fn duplicate_cloexec(&self) -> Result<OwnedStorage, DuplicateError> {
        match self {
            Self::Binary(storage) => Ok(OwnedStorage::Binary(storage.clone())),
            Self::DmaBuf(descriptor) => descriptor.duplicate_cloexec().map(OwnedStorage::DmaBuf),
        }
    }
}

impl BinaryStorage {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.planes.is_empty() {
            return Err(ValidationError::EmptyBinaryPlanes);
        }
        if self.planes.len() != 1 {
            return Err(ValidationError::UnsupportedBinaryPlaneCount(
                self.planes.len(),
            ));
        }
        let plane = self.planes[0];
        if plane.stride == 0 {
            return Err(ValidationError::ZeroBinaryStride);
        }
        if plane.offset >= self.data.len() as u64 {
            return Err(ValidationError::BinaryOffsetOutOfBounds {
                offset: plane.offset,
                data_size: self.data.len() as u64,
            });
        }
        Ok(())
    }

    fn validate_height(&self, coded_height: u32) -> Result<(), ValidationError> {
        let plane = self.planes[0];
        let last_row = plane
            .offset
            .checked_add(u64::from(plane.stride) * u64::from(coded_height.saturating_sub(1)))
            .ok_or(ValidationError::BinaryLastRowOutOfBounds {
                offset: u64::MAX,
                data_size: self.data.len() as u64,
            })?;
        if last_row >= self.data.len() as u64 {
            return Err(ValidationError::BinaryLastRowOutOfBounds {
                offset: last_row,
                data_size: self.data.len() as u64,
            });
        }
        Ok(())
    }
}

impl FrameDescriptor {
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.visible_rect
            .validate(self.coded_width, self.coded_height)?;
        self.storage.validate()?;
        if let Storage::Binary(storage) = &self.storage {
            storage.validate_height(self.coded_height)?;
        }

        if matches!(self.storage, Storage::Binary(_))
            && !matches!(self.acquire_sync, AcquireSync::Implicit)
        {
            return Err(ValidationError::BinaryStorageRequiresImplicitSync);
        }

        if let AcquireSync::SyncFile(sync_file) = &self.acquire_sync
            && sync_file.acquire_fence_fd < 0
        {
            return Err(ValidationError::NegativeAcquireFence(
                sync_file.acquire_fence_fd,
            ));
        }

        Ok(())
    }

    pub fn duplicate_cloexec(&self) -> Result<OwnedFrame, DuplicateError> {
        self.validate()?;
        let storage = self.storage.duplicate_cloexec()?;
        let acquire_sync = self.acquire_sync.duplicate_cloexec()?;

        Ok(OwnedFrame {
            coded_width: self.coded_width,
            coded_height: self.coded_height,
            visible_rect: self.visible_rect.clone(),
            storage,
            acquire_sync,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_binary_last_row_without_requiring_trailing_padding() {
        let frame = FrameDescriptor {
            coded_width: 1,
            coded_height: 2,
            visible_rect: Rect {
                x: 0,
                y: 0,
                width: 1,
                height: 2,
            },
            storage: Storage::Binary(BinaryStorage {
                data: vec![0; 10],
                planes: vec![BinaryPlane {
                    offset: 0,
                    stride: 9,
                }],
            }),
            acquire_sync: AcquireSync::Implicit,
        };

        assert_eq!(frame.validate(), Ok(()));
    }

    #[test]
    fn rejects_binary_storage_when_the_last_row_is_absent() {
        let frame = FrameDescriptor {
            coded_width: 1,
            coded_height: 2,
            visible_rect: Rect {
                x: 0,
                y: 0,
                width: 1,
                height: 2,
            },
            storage: Storage::Binary(BinaryStorage {
                data: vec![0; 9],
                planes: vec![BinaryPlane {
                    offset: 0,
                    stride: 9,
                }],
            }),
            acquire_sync: AcquireSync::Implicit,
        };

        assert_eq!(
            frame.validate(),
            Err(ValidationError::BinaryLastRowOutOfBounds {
                offset: 9,
                data_size: 9
            })
        );
    }
}

#[cfg(feature = "rustler")]
mod rustler_impl {
    use rustler::{Binary, Decoder, Encoder, Env, NewBinary, NifResult, Term};

    use super::{BinaryPlane, BinaryStorage, Storage};
    use crate::Descriptor;

    #[derive(rustler::NifStruct)]
    #[module = "VideoInterop.Binary"]
    struct BinaryStorageNif<'a> {
        data: Binary<'a>,
        planes: Vec<BinaryPlane>,
    }

    impl<'a> Decoder<'a> for Storage {
        fn decode(term: Term<'a>) -> NifResult<Self> {
            if let Ok(descriptor) = term.decode::<Descriptor>() {
                return Ok(Self::DmaBuf(descriptor));
            }

            term.decode::<BinaryStorageNif>().map(|storage| {
                Self::Binary(BinaryStorage {
                    data: storage.data.as_slice().to_vec(),
                    planes: storage.planes,
                })
            })
        }
    }

    impl Encoder for Storage {
        fn encode<'a>(&self, env: Env<'a>) -> Term<'a> {
            match self {
                Self::Binary(storage) => {
                    let mut data = NewBinary::new(env, storage.data.len());
                    data.as_mut_slice().copy_from_slice(&storage.data);
                    BinaryStorageNif {
                        data: data.into(),
                        planes: storage.planes.clone(),
                    }
                    .encode(env)
                }
                Self::DmaBuf(descriptor) => descriptor.encode(env),
            }
        }
    }
}
