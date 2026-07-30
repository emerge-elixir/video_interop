use crate::{
    AcquireSync, Descriptor, DuplicateError, OwnedAcquireSync, OwnedDescriptor, Rect,
    ValidationError,
};

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Storage {
    DmaBuf(Descriptor),
}

#[derive(Debug)]
#[non_exhaustive]
pub enum OwnedStorage {
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
            Self::DmaBuf(descriptor) => descriptor.validate(),
        }
    }

    pub fn duplicate_cloexec(&self) -> Result<OwnedStorage, DuplicateError> {
        match self {
            Self::DmaBuf(descriptor) => descriptor.duplicate_cloexec().map(OwnedStorage::DmaBuf),
        }
    }
}

impl FrameDescriptor {
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.visible_rect
            .validate(self.coded_width, self.coded_height)?;
        self.storage.validate()?;

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

#[cfg(feature = "rustler")]
mod rustler_impl {
    use rustler::{Decoder, Encoder, Env, NifResult, Term};

    use super::Storage;
    use crate::Descriptor;

    impl<'a> Decoder<'a> for Storage {
        fn decode(term: Term<'a>) -> NifResult<Self> {
            term.decode::<Descriptor>().map(Self::DmaBuf)
        }
    }

    impl Encoder for Storage {
        fn encode<'a>(&self, env: Env<'a>) -> Term<'a> {
            match self {
                Self::DmaBuf(descriptor) => descriptor.encode(env),
            }
        }
    }
}
