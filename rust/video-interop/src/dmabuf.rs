use std::{collections::BTreeSet, os::fd::OwnedFd};

use crate::{DuplicateError, Modifier, ValidationError, duplicate_fd_cloexec};

pub const AV_DRM_MAX_ENTRIES: usize = 4;

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
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, RawFd};

    use super::{Descriptor, Layer, Modifier, Object, Plane};
    use crate::duplicate_fd_cloexec;

    #[test]
    fn closes_earlier_duplicates_when_a_later_duplicate_fails() {
        let source = File::open("/dev/null").expect("open /dev/null");
        let descriptor = Descriptor {
            version: 1,
            objects: vec![
                Object {
                    fd: source.as_raw_fd(),
                    size: 4096,
                    modifier: Modifier::Implicit,
                },
                Object {
                    fd: source.as_raw_fd(),
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
        let mut first_duplicate: Option<RawFd> = None;
        let result = descriptor.duplicate_with(|fd| {
            calls += 1;
            if calls == 2 {
                return Err(io::Error::other("injected duplicate failure"));
            }

            let owned = duplicate_fd_cloexec(fd)?;
            first_duplicate = Some(owned.as_raw_fd());
            Ok(owned)
        });

        assert!(result.is_err());
        let duplicated = first_duplicate.expect("first duplicate");
        // SAFETY: F_GETFD only observes whether the recorded descriptor remains open.
        assert_eq!(unsafe { libc::fcntl(duplicated, libc::F_GETFD) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
    }
}
