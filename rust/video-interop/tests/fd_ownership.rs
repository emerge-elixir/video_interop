use std::{fs::File, os::fd::AsRawFd};

use video_interop::{
    AcquireSync, Descriptor, Layer, Modifier, Object, OwnedAcquireSync, Plane, SyncFile,
};

fn descriptor_with_fd(fd: i32) -> Descriptor {
    Descriptor {
        version: 1,
        objects: vec![Object {
            fd,
            size: 4096,
            modifier: Modifier::Implicit,
        }],
        layers: vec![Layer {
            fourcc: u32::from_le_bytes(*b"XR24"),
            planes: vec![Plane {
                object_index: 0,
                offset: 0,
                pitch: 256,
            }],
        }],
    }
}

#[test]
fn duplicates_borrowed_fds_with_cloexec() {
    let source = File::open("/dev/null").expect("open /dev/null");
    let source_fd = source.as_raw_fd();

    let owned = descriptor_with_fd(source_fd)
        .duplicate_cloexec()
        .expect("duplicate descriptor");
    let duplicated_fd = owned.objects[0].fd.as_raw_fd();

    assert_ne!(duplicated_fd, source_fd);
    // SAFETY: F_GETFD observes flags without taking ownership.
    let flags = unsafe { libc::fcntl(duplicated_fd, libc::F_GETFD) };
    assert!(flags >= 0);
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    // SAFETY: source remains owned by File.
    assert!(unsafe { libc::fcntl(source_fd, libc::F_GETFD) } >= 0);

    drop(owned);
    // SAFETY: this only verifies that RAII closed the recorded duplicate.
    assert_eq!(unsafe { libc::fcntl(duplicated_fd, libc::F_GETFD) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EBADF)
    );
    // SAFETY: the source descriptor must remain open.
    assert!(unsafe { libc::fcntl(source_fd, libc::F_GETFD) } >= 0);
}

#[test]
fn duplicates_acquire_sync_files_with_cloexec() {
    let source = File::open("/dev/null").expect("open /dev/null");
    let source_fd = source.as_raw_fd();
    let synchronization = AcquireSync::SyncFile(SyncFile {
        acquire_fence_fd: source_fd,
    });

    let owned = synchronization
        .duplicate_cloexec()
        .expect("duplicate acquire fence");
    let OwnedAcquireSync::SyncFile(acquire_fence_fd) = owned else {
        panic!("expected an owned sync file");
    };

    assert_ne!(acquire_fence_fd.as_raw_fd(), source_fd);
    // SAFETY: F_GETFD observes flags without taking ownership.
    let flags = unsafe { libc::fcntl(acquire_fence_fd.as_raw_fd(), libc::F_GETFD) };
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
}
