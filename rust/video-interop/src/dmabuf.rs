use std::{
    collections::BTreeSet,
    os::fd::{OwnedFd, RawFd},
};

use crate::{
    DmaBufAllocationSizeError, DuplicateError, Modifier, ValidationError, duplicate_fd_cloexec,
};

pub const AV_DRM_MAX_ENTRIES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DmaBufProbe {
    pub device: u64,
    pub inode: u64,
    pub allocation_size: u64,
}

/// Returns the complete allocation size exposed by a DMA-BUF fd.
///
/// This is not a visible image or plane span. The allocation size includes any exporter alignment
/// padding outside the final addressable plane byte. A descriptor must report the complete size
/// returned by the fd.
pub fn dmabuf_allocation_size(fd: RawFd) -> Result<u64, DmaBufAllocationSizeError> {
    probe_dmabuf(fd).map(|probe| probe.allocation_size)
}

pub(crate) fn probe_dmabuf(fd: RawFd) -> Result<DmaBufProbe, DmaBufAllocationSizeError> {
    // Keep inode identities and file offsets 64-bit on 32-bit Linux too.
    let mut stat = std::mem::MaybeUninit::<libc::stat64>::zeroed();
    // SAFETY: `stat` points to writable storage and this call does not take ownership of `fd`.
    if unsafe { libc::fstat64(fd, stat.as_mut_ptr()) } != 0 {
        return Err(DmaBufAllocationSizeError::Stat(
            std::io::Error::last_os_error(),
        ));
    }
    // SAFETY: fstat64 initialized the complete structure after returning success.
    let stat = unsafe { stat.assume_init() };

    // Linux DMA-BUF exporters expose their complete allocation through SEEK_END. Preserve the
    // shared file position when this fd also supports SEEK_CUR/SEEK_SET.
    let original_position = unsafe { libc::lseek64(fd, 0, libc::SEEK_CUR) };
    let allocation_end = unsafe { libc::lseek64(fd, 0, libc::SEEK_END) };
    let seek_error = (allocation_end < 0).then(std::io::Error::last_os_error);
    if original_position >= 0 && unsafe { libc::lseek64(fd, original_position, libc::SEEK_SET) } < 0
    {
        return Err(DmaBufAllocationSizeError::Restore(
            std::io::Error::last_os_error(),
        ));
    }
    if let Some(error) = seek_error {
        return Err(DmaBufAllocationSizeError::Seek(error));
    }

    let allocation_size =
        u64::try_from(allocation_end).expect("a non-negative off64_t always fits in u64");
    if allocation_size == 0 {
        return Err(DmaBufAllocationSizeError::Zero);
    }
    if stat.st_size < 0 {
        return Err(DmaBufAllocationSizeError::NegativeStat(stat.st_size));
    }
    if stat.st_size > 0 {
        let stat_size = u64::try_from(stat.st_size).expect("a positive off64_t always fits in u64");
        if stat_size != allocation_size {
            return Err(DmaBufAllocationSizeError::ProbeMismatch {
                stat: stat_size,
                seek_end: allocation_size,
            });
        }
    }

    Ok(DmaBufProbe {
        device: stat.st_dev,
        inode: stat.st_ino,
        allocation_size,
    })
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "rustler", derive(rustler::NifStruct))]
#[cfg_attr(feature = "rustler", module = "VideoInterop.DMABuf.Object")]
pub struct Object {
    pub fd: i32,
    pub size: u64,
    pub modifier: Modifier,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "rustler", derive(rustler::NifStruct))]
#[cfg_attr(feature = "rustler", module = "VideoInterop.DMABuf.Plane")]
pub struct Plane {
    pub object_index: u32,
    pub offset: u64,
    pub pitch: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "rustler", derive(rustler::NifStruct))]
#[cfg_attr(feature = "rustler", rustler(encode))]
#[cfg_attr(feature = "rustler", module = "VideoInterop.DMABuf.Layer")]
pub struct Layer {
    pub fourcc: u32,
    pub planes: Vec<Plane>,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "rustler", derive(rustler::NifStruct))]
#[cfg_attr(feature = "rustler", rustler(encode))]
#[cfg_attr(feature = "rustler", module = "VideoInterop.DMABuf.Descriptor")]
pub struct Descriptor {
    pub version: u32,
    pub objects: Vec<Object>,
    pub layers: Vec<Layer>,
}

#[derive(Debug)]
pub struct OwnedObject {
    pub fd: OwnedFd,
    pub size: u64,
    pub modifier: Modifier,
}

#[derive(Debug)]
pub struct OwnedDescriptor {
    pub version: u32,
    pub objects: Vec<OwnedObject>,
    pub layers: Vec<Layer>,
}

impl Descriptor {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.version != 1 {
            return Err(ValidationError::UnsupportedDescriptorVersion(self.version));
        }
        if self.objects.is_empty() {
            return Err(ValidationError::EmptyObjects);
        }
        if self.objects.len() > AV_DRM_MAX_ENTRIES {
            return Err(ValidationError::TooManyEntries {
                kind: "objects",
                actual: self.objects.len(),
                maximum: AV_DRM_MAX_ENTRIES,
            });
        }
        if self.layers.is_empty() {
            return Err(ValidationError::EmptyLayers);
        }
        if self.layers.len() > AV_DRM_MAX_ENTRIES {
            return Err(ValidationError::TooManyEntries {
                kind: "layers",
                actual: self.layers.len(),
                maximum: AV_DRM_MAX_ENTRIES,
            });
        }

        for (index, object) in self.objects.iter().enumerate() {
            if object.fd < 0 {
                return Err(ValidationError::NegativeFd {
                    index,
                    fd: object.fd,
                });
            }
            if object.size == 0 {
                return Err(ValidationError::ZeroObjectSize { index });
            }
        }

        for (layer_index, layer) in self.layers.iter().enumerate() {
            if layer.fourcc == 0 {
                return Err(ValidationError::InvalidFourcc { index: layer_index });
            }
            if layer.planes.is_empty() {
                return Err(ValidationError::EmptyPlanes { index: layer_index });
            }
            if layer.planes.len() > AV_DRM_MAX_ENTRIES {
                return Err(ValidationError::TooManyEntries {
                    kind: "planes in one layer",
                    actual: layer.planes.len(),
                    maximum: AV_DRM_MAX_ENTRIES,
                });
            }

            for (plane_index, plane) in layer.planes.iter().enumerate() {
                if plane.pitch == 0 {
                    return Err(ValidationError::ZeroPitch {
                        layer: layer_index,
                        plane: plane_index,
                    });
                }

                let object = self.objects.get(plane.object_index as usize).ok_or(
                    ValidationError::InvalidObjectIndex {
                        layer: layer_index,
                        plane: plane_index,
                        object_index: plane.object_index,
                        object_count: self.objects.len(),
                    },
                )?;

                if plane.offset >= object.size {
                    return Err(ValidationError::PlaneOffsetOutOfBounds {
                        layer: layer_index,
                        plane: plane_index,
                        object_index: plane.object_index,
                        offset: plane.offset,
                        object_size: object.size,
                    });
                }
            }
        }

        let total_planes = self.layers.iter().map(|layer| layer.planes.len()).sum();
        if total_planes > AV_DRM_MAX_ENTRIES {
            return Err(ValidationError::TooManyPlanes {
                actual: total_planes,
                maximum: AV_DRM_MAX_ENTRIES,
            });
        }

        let referenced = self
            .layers
            .iter()
            .flat_map(|layer| layer.planes.iter())
            .map(|plane| plane.object_index as usize)
            .collect::<BTreeSet<_>>();
        if let Some(index) = (0..self.objects.len()).find(|index| !referenced.contains(index)) {
            return Err(ValidationError::UnreferencedObject { index });
        }

        Ok(())
    }

    pub fn duplicate_cloexec(&self) -> Result<OwnedDescriptor, DuplicateError> {
        self.duplicate_with(duplicate_fd_cloexec)
    }

    fn duplicate_with<F>(&self, mut duplicate: F) -> Result<OwnedDescriptor, DuplicateError>
    where
        F: FnMut(i32) -> std::io::Result<OwnedFd>,
    {
        self.validate()?;

        let objects = self
            .objects
            .iter()
            .enumerate()
            .map(|(index, object)| {
                duplicate(object.fd)
                    .map(|fd| OwnedObject {
                        fd,
                        size: object.size,
                        modifier: object.modifier,
                    })
                    .map_err(|source| DuplicateError::DuplicateObjectFd {
                        index,
                        fd: object.fd,
                        source,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(OwnedDescriptor {
            version: self.version,
            objects,
            layers: self.layers.clone(),
        })
    }
}

#[cfg(feature = "rustler")]
mod rustler_impl {
    use rustler::{Atom, Decoder, Error, ListIterator, NifResult, Term};

    use super::{AV_DRM_MAX_ENTRIES, Descriptor, Layer};

    mod atoms {
        rustler::atoms! {
            atom_struct = "__struct__",
            version,
            objects,
            layers,
            fourcc,
            planes
        }
    }

    impl<'a> Decoder<'a> for Layer {
        fn decode(term: Term<'a>) -> NifResult<Self> {
            ensure_struct(term, "Elixir.VideoInterop.DMABuf.Layer")?;

            Ok(Self {
                fourcc: term.map_get(atoms::fourcc())?.decode()?,
                planes: decode_bounded_list(term.map_get(atoms::planes())?)?,
            })
        }
    }

    impl<'a> Decoder<'a> for Descriptor {
        fn decode(term: Term<'a>) -> NifResult<Self> {
            ensure_struct(term, "Elixir.VideoInterop.DMABuf.Descriptor")?;

            Ok(Self {
                version: term.map_get(atoms::version())?.decode()?,
                objects: decode_bounded_list(term.map_get(atoms::objects())?)?,
                layers: decode_bounded_list(term.map_get(atoms::layers())?)?,
            })
        }
    }

    fn ensure_struct(term: Term<'_>, module: &str) -> NifResult<()> {
        let actual: Atom = term.map_get(atoms::atom_struct())?.decode()?;
        let expected = Atom::from_str(term.get_env(), module)?;

        if actual == expected {
            Ok(())
        } else {
            Err(Error::BadArg)
        }
    }

    fn decode_bounded_list<'a, T>(term: Term<'a>) -> NifResult<Vec<T>>
    where
        T: Decoder<'a>,
    {
        let iterator: ListIterator<'a> = term.decode()?;
        let mut values = Vec::with_capacity(AV_DRM_MAX_ENTRIES);

        for item in iterator {
            if values.len() == AV_DRM_MAX_ENTRIES {
                return Err(Error::BadArg);
            }
            values.push(item.decode()?);
        }

        Ok(values)
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    use super::{Descriptor, Layer, Modifier, Object, Plane, dmabuf_allocation_size};
    use crate::{DmaBufAllocationSizeError, duplicate_fd_cloexec};

    #[cfg(target_os = "linux")]
    fn memfd(size: i64) -> OwnedFd {
        let raw =
            unsafe { libc::memfd_create(c"video-interop-size-test".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(raw >= 0);
        assert_eq!(unsafe { libc::ftruncate64(raw, size) }, 0);
        // SAFETY: memfd_create returned one owned descriptor.
        unsafe { OwnedFd::from_raw_fd(raw) }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn allocation_size_reports_complete_size_and_restores_position() {
        let fd = memfd(4_096);
        assert_eq!(
            unsafe { libc::lseek64(fd.as_raw_fd(), 17, libc::SEEK_SET) },
            17
        );

        assert_eq!(dmabuf_allocation_size(fd.as_raw_fd()).unwrap(), 4_096);
        assert_eq!(
            unsafe { libc::lseek64(fd.as_raw_fd(), 0, libc::SEEK_CUR) },
            17
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn allocation_size_and_identity_preserve_large_file_values() {
        use std::os::unix::fs::MetadataExt;

        let position = i64::from(i32::MAX) + 1;
        let size = position + 4096;
        // ftruncate64 creates a sparse memfd; no multi-gigabyte buffer is allocated.
        let fd = memfd(size);
        assert_eq!(
            unsafe { libc::lseek64(fd.as_raw_fd(), position, libc::SEEK_SET) },
            position
        );

        let probe = super::probe_dmabuf(fd.as_raw_fd()).unwrap();
        assert_eq!(probe.allocation_size, size as u64);
        assert_eq!(
            unsafe { libc::lseek64(fd.as_raw_fd(), 0, libc::SEEK_CUR) },
            position
        );

        let metadata = std::fs::File::from(fd).metadata().unwrap();
        assert_eq!(probe.device, metadata.dev());
        assert_eq!(probe.inode, metadata.ino());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn allocation_size_rejects_zero_and_nonseekable_fds() {
        let empty = memfd(0);
        assert!(matches!(
            dmabuf_allocation_size(empty.as_raw_fd()),
            Err(DmaBufAllocationSizeError::Zero)
        ));

        let mut pipe = [-1; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        // SAFETY: pipe2 returned two owned descriptors.
        let read = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
        // SAFETY: pipe2 returned two owned descriptors.
        let _write = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
        assert!(matches!(
            dmabuf_allocation_size(read.as_raw_fd()),
            Err(DmaBufAllocationSizeError::Seek(_))
        ));
    }

    #[test]
    fn closes_earlier_duplicates_when_a_later_duplicate_fails() {
        let mut pipe = [-1; 2];
        assert_eq!(
            unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        // SAFETY: pipe2 returned two owned descriptors.
        let read = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
        // SAFETY: pipe2 returned two owned descriptors.
        let write = unsafe { OwnedFd::from_raw_fd(pipe[1]) };

        let descriptor = Descriptor {
            version: 1,
            objects: vec![
                Object {
                    fd: write.as_raw_fd(),
                    size: 4096,
                    modifier: Modifier::Implicit,
                },
                Object {
                    fd: write.as_raw_fd(),
                    size: 4096,
                    modifier: Modifier::Implicit,
                },
            ],
            layers: vec![Layer {
                fourcc: u32::from_le_bytes(*b"XR24"),
                planes: vec![
                    Plane {
                        object_index: 0,
                        offset: 0,
                        pitch: 256,
                    },
                    Plane {
                        object_index: 1,
                        offset: 0,
                        pitch: 256,
                    },
                ],
            }],
        };

        let mut calls = 0;
        let result = descriptor.duplicate_with(|fd| {
            calls += 1;
            if calls == 2 {
                Err(io::Error::other("injected duplicate failure"))
            } else {
                duplicate_fd_cloexec(fd)
            }
        });

        assert!(result.is_err());
        assert_eq!(calls, 2);
        drop(write);

        let mut byte = 0_u8;
        // SAFETY: read points to one writable byte and the pipe read descriptor is still owned.
        let read_count =
            unsafe { libc::read(read.as_raw_fd(), std::ptr::addr_of_mut!(byte).cast(), 1) };
        assert_eq!(
            read_count, 0,
            "the failed operation leaked a duplicated write fd"
        );
    }
}
