use std::io;
use std::os::fd::{FromRawFd, OwnedFd};

pub(crate) fn duplicate_fd_cloexec(fd: i32) -> io::Result<OwnedFd> {
    // SAFETY: fcntl borrows the input descriptor. On success it returns a new,
    // independently owned descriptor with FD_CLOEXEC already set.
    let duplicated = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicated < 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: F_DUPFD_CLOEXEC returned a fresh descriptor which has not been
    // wrapped or transferred elsewhere.
    Ok(unsafe { OwnedFd::from_raw_fd(duplicated) })
}
