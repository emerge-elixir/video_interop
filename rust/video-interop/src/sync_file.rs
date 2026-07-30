use std::os::fd::OwnedFd;

use crate::{DuplicateError, duplicate_fd_cloexec};

#[derive(Clone, Debug)]
#[cfg_attr(feature = "rustler", derive(rustler::NifStruct))]
#[cfg_attr(feature = "rustler", module = "VideoInterop.SyncFile")]
pub struct SyncFile {
    pub acquire_fence_fd: i32,
}

#[derive(Clone, Debug)]
pub enum AcquireSync {
    Implicit,
    SyncFile(SyncFile),
}

#[derive(Debug)]
pub enum OwnedAcquireSync {
    Implicit,
    SyncFile(OwnedFd),
}

impl SyncFile {
    pub fn duplicate_cloexec(&self) -> Result<OwnedFd, DuplicateError> {
        if self.acquire_fence_fd < 0 {
            return Err(DuplicateError::NegativeAcquireFence(self.acquire_fence_fd));
        }

        duplicate_fd_cloexec(self.acquire_fence_fd).map_err(|source| {
            DuplicateError::DuplicateAcquireFence {
                fd: self.acquire_fence_fd,
                source,
            }
        })
    }
}

impl AcquireSync {
    pub fn duplicate_cloexec(&self) -> Result<OwnedAcquireSync, DuplicateError> {
        match self {
            Self::Implicit => Ok(OwnedAcquireSync::Implicit),
            Self::SyncFile(sync_file) => sync_file
                .duplicate_cloexec()
                .map(OwnedAcquireSync::SyncFile),
        }
    }
}

#[cfg(feature = "rustler")]
mod rustler_impl {
    use rustler::{Decoder, Encoder, Env, Error, NifResult, Term};

    use super::{AcquireSync, SyncFile};

    mod atoms {
        rustler::atoms! {
            implicit
        }
    }

    impl<'a> Decoder<'a> for AcquireSync {
        fn decode(term: Term<'a>) -> NifResult<Self> {
            if let Ok(atom) = term.decode::<rustler::Atom>() {
                return if atom == atoms::implicit() {
                    Ok(Self::Implicit)
                } else {
                    Err(Error::BadArg)
                };
            }

            term.decode::<SyncFile>().map(Self::SyncFile)
        }
    }

    impl Encoder for AcquireSync {
        fn encode<'a>(&self, env: Env<'a>) -> Term<'a> {
            match self {
                Self::Implicit => atoms::implicit().encode(env),
                Self::SyncFile(sync_file) => sync_file.encode(env),
            }
        }
    }
}
