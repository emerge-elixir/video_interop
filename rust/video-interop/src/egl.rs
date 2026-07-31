//! Dynamically loaded EGL native-fence synchronization helpers.
//!
//! This module deliberately contains no link-time EGL dependency. Callers load
//! entry points from their active EGL implementation and retain responsibility
//! for using every handle on the thread/display that created it.

use std::{
    ffi::c_void,
    io,
    os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd},
    ptr,
    time::Duration,
};

pub type Display = *mut c_void;
pub type Sync = *mut c_void;
pub type Boolean = u32;
pub type Enum = u32;
pub type Int = i32;
pub type Attrib = isize;
pub type Time = u64;

pub const FALSE: Boolean = 0;
pub const NONE: Int = 0x3038;
pub const SUCCESS: Int = 0x3000;
pub const SYNC_NATIVE_FENCE_ANDROID: Enum = 0x3144;
pub const SYNC_NATIVE_FENCE_FD_ANDROID: Int = 0x3145;
pub const NO_NATIVE_FENCE_FD_ANDROID: Int = -1;
pub const CONDITION_SATISFIED: Int = 0x30F6;
pub const TIMEOUT_EXPIRED: Int = 0x30F5;
pub const FOREVER: Time = u64::MAX;

pub type CreateSyncKhr = unsafe extern "system" fn(Display, Enum, *const Int) -> Sync;
pub type CreateSyncCore = unsafe extern "system" fn(Display, Enum, *const Attrib) -> Sync;
pub type DestroySyncKhr = unsafe extern "system" fn(Display, Sync) -> Boolean;
pub type DestroySyncCore = unsafe extern "system" fn(Display, Sync) -> Boolean;
pub type ClientWaitSyncKhr = unsafe extern "system" fn(Display, Sync, Int, Time) -> Int;
pub type ClientWaitSyncCore = unsafe extern "system" fn(Display, Sync, Int, Time) -> Int;
// EGL_KHR_wait_sync specifies EGLint while EGL 1.5 core specifies EGLBoolean.
pub type WaitSyncKhr = unsafe extern "system" fn(Display, Sync, Int) -> Int;
pub type WaitSyncCore = unsafe extern "system" fn(Display, Sync, Int) -> Boolean;
pub type DupNativeFenceFd = unsafe extern "system" fn(Display, Sync) -> Int;
pub type GetError = unsafe extern "system" fn() -> Int;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncAbi {
    Khr,
    Core15,
}

#[derive(Clone, Copy, Default)]
struct KhrFunctionBundle {
    create: Option<CreateSyncKhr>,
    destroy: Option<DestroySyncKhr>,
    client_wait: Option<ClientWaitSyncKhr>,
    server_wait: Option<WaitSyncKhr>,
}

#[derive(Clone, Copy, Default)]
struct CoreFunctionBundle {
    create: Option<CreateSyncCore>,
    destroy: Option<DestroySyncCore>,
    client_wait: Option<ClientWaitSyncCore>,
    server_wait: Option<WaitSyncCore>,
}

/// Unselected entry points loaded from an EGL implementation.
///
/// KHR and core symbols stay in separate bundles until extension/version
/// checks choose one complete, compatible ABI.
#[derive(Clone, Copy, Default)]
pub struct NativeFenceFunctionLoader {
    khr: KhrFunctionBundle,
    core: CoreFunctionBundle,
    duplicate: Option<DupNativeFenceFd>,
    get_error: Option<GetError>,
}

#[derive(Clone, Copy)]
enum SelectedFunctions {
    Khr(KhrFunctionBundle),
    Core(CoreFunctionBundle),
}

/// A selected, internally consistent EGL synchronization ABI.
#[derive(Clone, Copy)]
pub struct NativeFenceFunctions {
    selected: SelectedFunctions,
    duplicate: Option<DupNativeFenceFd>,
    get_error: Option<GetError>,
    capabilities: NativeFenceCapabilities,
}

impl std::fmt::Debug for NativeFenceFunctions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeFenceFunctions")
            .field("abi", &self.sync_abi())
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeFenceCapabilities {
    pub producer_export: bool,
    pub consumer_import: bool,
    pub server_wait: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SyncCreateError {
    #[error("EGL native-fence creation is unsupported")]
    Unsupported,
    #[error("EGL native-fence creation failed (EGL error {egl_error:#x})")]
    Failed { egl_error: Int },
}

#[derive(Debug, thiserror::Error)]
pub enum FenceDuplicateError {
    #[error("EGL native-fence duplication is unsupported")]
    Unsupported,
    #[error("EGL native-fence duplication failed (EGL error {egl_error:#x})")]
    Failed { egl_error: Int },
    #[error("failed to set FD_CLOEXEC on duplicated native fence: {0}")]
    Cloexec(io::Error),
}

#[must_use = "EGL sync handles must be destroyed or retained until display teardown"]
#[derive(Debug)]
pub struct SyncHandle {
    display: Display,
    raw: Sync,
}

impl SyncHandle {
    pub fn display(&self) -> Display {
        self.display
    }

    pub fn as_raw(&self) -> Sync {
        self.raw
    }
}

#[derive(Debug)]
pub struct DestroyError {
    pub handle: SyncHandle,
    pub egl_error: Int,
}

impl std::fmt::Display for DestroyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "EGL sync destruction failed (EGL error {:#x})",
            self.egl_error
        )
    }
}

impl std::error::Error for DestroyError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerWaitOutcome {
    Queued,
    Unsupported,
    Failed { egl_error: Int },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientWaitOutcome {
    Satisfied,
    Timeout,
    Unsupported,
    Failed { status: Int, egl_error: Int },
}

#[derive(Debug)]
pub enum SyncFilePollOutcome {
    Signaled,
    Timeout,
    Error(io::Error),
}

/// Parse an EGL/GL extension string without accepting token prefixes.
pub fn has_extension(extensions: &str, wanted: &str) -> bool {
    !wanted.is_empty()
        && extensions
            .split_ascii_whitespace()
            .any(|token| token == wanted)
}

impl NativeFenceFunctions {
    /// Load native-fence entry points without creating a link-time EGL dependency.
    ///
    /// `load` should try both the EGL shared-library symbol table and
    /// `eglGetProcAddress`. Loading does not select or mix an ABI; callers must
    /// use [`NativeFenceFunctionLoader::select_producer`] or
    /// [`NativeFenceFunctionLoader::select_consumer`] after querying the active
    /// display's extensions and version.
    ///
    /// # Safety
    /// Every non-null address returned by `load` must have the ABI implied by
    /// its requested symbol name and remain valid while the selected table is
    /// used.
    pub unsafe fn load_with(
        mut load: impl FnMut(&str) -> *const c_void,
    ) -> NativeFenceFunctionLoader {
        NativeFenceFunctionLoader {
            khr: KhrFunctionBundle {
                create: unsafe { load_function(load("eglCreateSyncKHR")) },
                destroy: unsafe { load_function(load("eglDestroySyncKHR")) },
                client_wait: unsafe { load_function(load("eglClientWaitSyncKHR")) },
                server_wait: unsafe { load_function(load("eglWaitSyncKHR")) },
            },
            core: CoreFunctionBundle {
                create: unsafe { load_function(load("eglCreateSync")) },
                destroy: unsafe { load_function(load("eglDestroySync")) },
                client_wait: unsafe { load_function(load("eglClientWaitSync")) },
                server_wait: unsafe { load_function(load("eglWaitSync")) },
            },
            duplicate: unsafe { load_function(load("eglDupNativeFenceFDANDROID")) },
            get_error: unsafe { load_function(load("eglGetError")) },
        }
    }

    pub fn sync_abi(&self) -> SyncAbi {
        match self.selected {
            SelectedFunctions::Khr(_) => SyncAbi::Khr,
            SelectedFunctions::Core(_) => SyncAbi::Core15,
        }
    }

    pub fn capabilities(&self) -> NativeFenceCapabilities {
        self.capabilities
    }

    /// Create an export fence. No file descriptor is transferred on this path.
    ///
    /// # Safety
    /// `display` must be current and compatible with the loaded EGL functions.
    pub unsafe fn create_export_fence(
        &self,
        display: Display,
    ) -> Result<SyncHandle, SyncCreateError> {
        if !self.capabilities.producer_export {
            return Err(SyncCreateError::Unsupported);
        }
        unsafe { self.create(display, None) }
    }

    /// Import `fence`. EGL receives ownership before every create call and Rust
    /// never closes or reconstructs it, including when EGL reports failure.
    ///
    /// # Safety
    /// `display` must be current and compatible with the loaded EGL functions.
    pub unsafe fn import_sync_file(
        &self,
        display: Display,
        fence: OwnedFd,
    ) -> Result<SyncHandle, SyncCreateError> {
        if !self.capabilities.consumer_import {
            return Err(SyncCreateError::Unsupported);
        }
        let transferred_fd = fence.into_raw_fd();
        unsafe { self.create(display, Some(transferred_fd)) }
    }

    unsafe fn create(
        &self,
        display: Display,
        fence_fd: Option<Int>,
    ) -> Result<SyncHandle, SyncCreateError> {
        let raw = match self.selected {
            SelectedFunctions::Khr(bundle) => {
                let Some(create) = bundle.create else {
                    return Err(SyncCreateError::Unsupported);
                };
                let import_attrs;
                let export_attrs = [NONE];
                let attrs = if let Some(fence_fd) = fence_fd {
                    import_attrs = [SYNC_NATIVE_FENCE_FD_ANDROID, fence_fd, NONE];
                    import_attrs.as_ptr()
                } else {
                    export_attrs.as_ptr()
                };
                unsafe { create(display, SYNC_NATIVE_FENCE_ANDROID, attrs) }
            }
            SelectedFunctions::Core(bundle) => {
                let Some(create) = bundle.create else {
                    return Err(SyncCreateError::Unsupported);
                };
                let import_attrs;
                let export_attrs = [NONE as Attrib];
                let attrs = if let Some(fence_fd) = fence_fd {
                    import_attrs = [
                        SYNC_NATIVE_FENCE_FD_ANDROID as Attrib,
                        fence_fd as Attrib,
                        NONE as Attrib,
                    ];
                    import_attrs.as_ptr()
                } else {
                    export_attrs.as_ptr()
                };
                unsafe { create(display, SYNC_NATIVE_FENCE_ANDROID, attrs) }
            }
        };
        if raw.is_null() {
            Err(SyncCreateError::Failed {
                egl_error: unsafe { self.take_error() },
            })
        } else {
            Ok(SyncHandle { display, raw })
        }
    }

    /// Duplicate the native sync-file and enforce close-on-exec.
    ///
    /// # Safety
    /// The handle must still belong to this EGL display.
    pub unsafe fn duplicate_fence(
        &self,
        handle: &SyncHandle,
    ) -> Result<OwnedFd, FenceDuplicateError> {
        if !self.capabilities.producer_export {
            return Err(FenceDuplicateError::Unsupported);
        }
        let Some(duplicate) = self.duplicate else {
            return Err(FenceDuplicateError::Unsupported);
        };
        let fd = unsafe { duplicate(handle.display, handle.raw) };
        if fd < 0 {
            return Err(FenceDuplicateError::Failed {
                egl_error: unsafe { self.take_error() },
            });
        }
        // SAFETY: EGL returned a fresh caller-owned descriptor.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let flags = unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_GETFD) };
        if flags < 0
            || unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) }
                < 0
        {
            return Err(FenceDuplicateError::Cloexec(io::Error::last_os_error()));
        }
        Ok(owned)
    }

    /// Destroy a sync. Failure returns the still-owned handle for deferred retry.
    ///
    /// # Safety
    /// The display must remain valid.
    pub unsafe fn destroy(&self, handle: SyncHandle) -> Result<(), DestroyError> {
        let destroyed = match self.selected {
            SelectedFunctions::Khr(bundle) => bundle
                .destroy
                .is_some_and(|destroy| unsafe { destroy(handle.display, handle.raw) } != FALSE),
            SelectedFunctions::Core(bundle) => bundle
                .destroy
                .is_some_and(|destroy| unsafe { destroy(handle.display, handle.raw) } != FALSE),
        };
        if destroyed {
            Ok(())
        } else {
            Err(DestroyError {
                handle,
                egl_error: unsafe { self.take_error() },
            })
        }
    }

    /// Queue a GPU-side dependency. Flags are intentionally always zero.
    ///
    /// # Safety
    /// The sync/display must be valid on the calling render thread.
    pub unsafe fn wait_server(&self, handle: &SyncHandle) -> ServerWaitOutcome {
        if !self.capabilities.server_wait {
            return ServerWaitOutcome::Unsupported;
        }
        let succeeded = match self.selected {
            SelectedFunctions::Khr(bundle) => {
                let Some(wait) = bundle.server_wait else {
                    return ServerWaitOutcome::Unsupported;
                };
                // EGL_KHR_wait_sync returns EGLint, not EGLBoolean.
                (unsafe { wait(handle.display, handle.raw, 0) }) != FALSE as Int
            }
            SelectedFunctions::Core(bundle) => {
                let Some(wait) = bundle.server_wait else {
                    return ServerWaitOutcome::Unsupported;
                };
                (unsafe { wait(handle.display, handle.raw, 0) }) != FALSE
            }
        };
        if succeeded {
            ServerWaitOutcome::Queued
        } else {
            ServerWaitOutcome::Failed {
                egl_error: unsafe { self.take_error() },
            }
        }
    }

    /// Wait on the render thread for at most `timeout`. Flags are zero.
    ///
    /// # Safety
    /// The sync/display must be valid on the calling render thread.
    pub unsafe fn wait_client(&self, handle: &SyncHandle, timeout: Duration) -> ClientWaitOutcome {
        // Keep every finite Rust duration distinct from EGL_FOREVER.
        let nanos = timeout.as_nanos().min(u128::from(FOREVER - 1)) as u64;
        let status = match self.selected {
            SelectedFunctions::Khr(bundle) => {
                let Some(wait) = bundle.client_wait else {
                    return ClientWaitOutcome::Unsupported;
                };
                unsafe { wait(handle.display, handle.raw, 0, nanos) }
            }
            SelectedFunctions::Core(bundle) => {
                let Some(wait) = bundle.client_wait else {
                    return ClientWaitOutcome::Unsupported;
                };
                unsafe { wait(handle.display, handle.raw, 0, nanos) }
            }
        };
        match status {
            CONDITION_SATISFIED => ClientWaitOutcome::Satisfied,
            TIMEOUT_EXPIRED => ClientWaitOutcome::Timeout,
            other => ClientWaitOutcome::Failed {
                status: other,
                egl_error: unsafe { self.take_error() },
            },
        }
    }

    unsafe fn take_error(&self) -> Int {
        self.get_error
            .map_or(SUCCESS, |get_error| unsafe { get_error() })
    }
}

impl NativeFenceFunctionLoader {
    /// Select the KHR ABI required by `EGL_ANDROID_native_fence_sync` export.
    /// Core and KHR functions are never combined on this path.
    pub fn select_producer(&self, egl_extensions: &str) -> Option<NativeFenceFunctions> {
        let extensions_ok = has_extension(egl_extensions, "EGL_ANDROID_native_fence_sync")
            && has_extension(egl_extensions, "EGL_KHR_fence_sync");
        let complete =
            self.khr.create.is_some() && self.khr.destroy.is_some() && self.duplicate.is_some();
        (extensions_ok && complete).then_some(NativeFenceFunctions {
            selected: SelectedFunctions::Khr(self.khr),
            duplicate: self.duplicate,
            get_error: self.get_error,
            capabilities: NativeFenceCapabilities {
                producer_export: true,
                consumer_import: false,
                server_wait: false,
            },
        })
    }

    /// Select one complete consumer ABI after checking the active display and
    /// current client API. KHR is preferred when its required extensions and
    /// base functions exist; otherwise a complete core table is allowed only
    /// on EGL 1.5 or newer. `core_server_wait_supported` must reflect the EGL
    /// 1.5 client-API requirements (for example OpenGL 3.2+, OpenGL ES 3.0+,
    /// or the corresponding sync extension), not merely symbol availability.
    pub fn select_consumer(
        &self,
        egl_extensions: &str,
        gl_extensions: &str,
        egl_15_or_newer: bool,
        core_server_wait_supported: bool,
    ) -> Option<NativeFenceFunctions> {
        let native = has_extension(egl_extensions, "EGL_ANDROID_native_fence_sync");
        let khr_extensions = native && has_extension(egl_extensions, "EGL_KHR_fence_sync");
        let khr_complete = self.khr.create.is_some()
            && self.khr.destroy.is_some()
            && self.khr.client_wait.is_some();
        if khr_extensions && khr_complete {
            let server_wait = self.khr.server_wait.is_some()
                && has_extension(egl_extensions, "EGL_KHR_wait_sync")
                && has_extension(gl_extensions, "GL_OES_EGL_sync");
            let mut selected = self.khr;
            if !server_wait {
                selected.server_wait = None;
            }
            return Some(NativeFenceFunctions {
                selected: SelectedFunctions::Khr(selected),
                duplicate: self.duplicate,
                get_error: self.get_error,
                capabilities: NativeFenceCapabilities {
                    producer_export: false,
                    consumer_import: true,
                    server_wait,
                },
            });
        }

        let core_complete = self.core.create.is_some()
            && self.core.destroy.is_some()
            && self.core.client_wait.is_some();
        if native && egl_15_or_newer && core_complete {
            let server_wait = self.core.server_wait.is_some() && core_server_wait_supported;
            let mut selected = self.core;
            if !server_wait {
                selected.server_wait = None;
            }
            return Some(NativeFenceFunctions {
                selected: SelectedFunctions::Core(selected),
                duplicate: None,
                get_error: self.get_error,
                capabilities: NativeFenceCapabilities {
                    producer_export: false,
                    consumer_import: true,
                    server_wait,
                },
            });
        }

        None
    }
}

unsafe fn load_function<T: Copy>(address: *const c_void) -> Option<T> {
    (!address.is_null()).then(|| unsafe { std::mem::transmute_copy::<*const c_void, T>(&address) })
}

/// Bounded CPU wait for a sync-file when EGL native-fence import is unavailable.
pub fn poll_sync_file(fence: &OwnedFd, timeout: Duration) -> SyncFilePollOutcome {
    let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    let mut poll_fd = libc::pollfd {
        fd: fence.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(ptr::addr_of_mut!(poll_fd), 1, timeout_ms) };
    if result < 0 {
        return SyncFilePollOutcome::Error(io::Error::last_os_error());
    }
    if result == 0 {
        return SyncFilePollOutcome::Timeout;
    }
    if poll_fd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
        return SyncFilePollOutcome::Error(io::Error::other(format!(
            "sync-file poll returned revents={:#x}",
            poll_fd.revents
        )));
    }
    if poll_fd.revents & libc::POLLIN != 0 {
        SyncFilePollOutcome::Signaled
    } else {
        SyncFilePollOutcome::Error(io::Error::other(format!(
            "sync-file poll returned unexpected revents={:#x}",
            poll_fd.revents
        )))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        os::fd::{AsRawFd, FromRawFd},
        sync::{
            Mutex,
            atomic::{AtomicI32, AtomicU64, AtomicUsize, Ordering},
        },
    };

    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());
    static CREATE_FD: AtomicI32 = AtomicI32::new(-2);
    static CREATE_RESULT: AtomicUsize = AtomicUsize::new(1);
    static DESTROY_CALLS: AtomicUsize = AtomicUsize::new(0);
    static CLIENT_WAIT_RESULT: AtomicI32 = AtomicI32::new(CONDITION_SATISFIED);
    static CLIENT_WAIT_TIMEOUT: AtomicU64 = AtomicU64::new(0);
    static KHR_SERVER_WAIT_RESULT: AtomicI32 = AtomicI32::new(1);
    static CORE_SERVER_WAIT_RESULT: AtomicUsize = AtomicUsize::new(1);
    static KHR_SERVER_CALLS: AtomicUsize = AtomicUsize::new(0);
    static CORE_SERVER_CALLS: AtomicUsize = AtomicUsize::new(0);
    static DUPLICATE_FD: AtomicI32 = AtomicI32::new(-1);

    unsafe extern "system" fn create_khr(
        _display: Display,
        _kind: Enum,
        attrs: *const Int,
    ) -> Sync {
        let first = unsafe { *attrs };
        CREATE_FD.store(
            if first == NONE {
                NO_NATIVE_FENCE_FD_ANDROID
            } else {
                unsafe { *attrs.add(1) }
            },
            Ordering::SeqCst,
        );
        CREATE_RESULT.load(Ordering::SeqCst) as Sync
    }

    unsafe extern "system" fn create_core(
        _display: Display,
        _kind: Enum,
        attrs: *const Attrib,
    ) -> Sync {
        let first = unsafe { *attrs };
        CREATE_FD.store(
            if first == NONE as Attrib {
                NO_NATIVE_FENCE_FD_ANDROID
            } else {
                unsafe { *attrs.add(1) as Int }
            },
            Ordering::SeqCst,
        );
        CREATE_RESULT.load(Ordering::SeqCst) as Sync
    }

    unsafe extern "system" fn destroy(_display: Display, _sync: Sync) -> Boolean {
        DESTROY_CALLS.fetch_add(1, Ordering::SeqCst);
        1
    }

    unsafe extern "system" fn client_wait(
        _display: Display,
        _sync: Sync,
        _flags: Int,
        timeout: Time,
    ) -> Int {
        CLIENT_WAIT_TIMEOUT.store(timeout, Ordering::SeqCst);
        CLIENT_WAIT_RESULT.load(Ordering::SeqCst)
    }

    unsafe extern "system" fn server_wait_khr(_display: Display, _sync: Sync, _flags: Int) -> Int {
        KHR_SERVER_CALLS.fetch_add(1, Ordering::SeqCst);
        KHR_SERVER_WAIT_RESULT.load(Ordering::SeqCst)
    }

    unsafe extern "system" fn server_wait_core(
        _display: Display,
        _sync: Sync,
        _flags: Int,
    ) -> Boolean {
        CORE_SERVER_CALLS.fetch_add(1, Ordering::SeqCst);
        CORE_SERVER_WAIT_RESULT.load(Ordering::SeqCst) as Boolean
    }

    unsafe extern "system" fn duplicate(_display: Display, _sync: Sync) -> Int {
        DUPLICATE_FD.swap(-1, Ordering::SeqCst)
    }

    fn address(name: &str) -> *const c_void {
        match name {
            "eglCreateSyncKHR" => create_khr as *const () as *const c_void,
            "eglDestroySyncKHR" => destroy as *const () as *const c_void,
            "eglClientWaitSyncKHR" => client_wait as *const () as *const c_void,
            "eglWaitSyncKHR" => server_wait_khr as *const () as *const c_void,
            "eglCreateSync" => create_core as *const () as *const c_void,
            "eglDestroySync" => destroy as *const () as *const c_void,
            "eglClientWaitSync" => client_wait as *const () as *const c_void,
            "eglWaitSync" => server_wait_core as *const () as *const c_void,
            "eglDupNativeFenceFDANDROID" => duplicate as *const () as *const c_void,
            _ => ptr::null(),
        }
    }

    fn loader_with(symbols: &[&str]) -> NativeFenceFunctionLoader {
        unsafe {
            NativeFenceFunctions::load_with(|name| {
                if symbols.contains(&name) {
                    address(name)
                } else {
                    ptr::null()
                }
            })
        }
    }

    fn khr_consumer() -> NativeFenceFunctions {
        loader_with(&[
            "eglCreateSyncKHR",
            "eglDestroySyncKHR",
            "eglClientWaitSyncKHR",
            "eglWaitSyncKHR",
        ])
        .select_consumer(
            "EGL_ANDROID_native_fence_sync EGL_KHR_fence_sync EGL_KHR_wait_sync",
            "GL_OES_EGL_sync",
            false,
            false,
        )
        .expect("KHR consumer")
    }

    fn khr_producer() -> NativeFenceFunctions {
        loader_with(&[
            "eglCreateSyncKHR",
            "eglDestroySyncKHR",
            "eglDupNativeFenceFDANDROID",
        ])
        .select_producer("EGL_ANDROID_native_fence_sync EGL_KHR_fence_sync")
        .expect("KHR producer")
    }

    #[test]
    fn extension_matching_requires_complete_tokens() {
        let extensions = "EGL_KHR_fence_sync EGL_ANDROID_native_fence_sync";
        assert!(has_extension(extensions, "EGL_KHR_fence_sync"));
        assert!(!has_extension(extensions, "EGL_KHR_fence"));
        assert!(!has_extension(extensions, ""));
    }

    #[test]
    fn selection_rejects_missing_and_mixed_symbol_bundles() {
        let mixed = loader_with(&[
            "eglCreateSyncKHR",
            "eglDestroySync",
            "eglClientWaitSync",
            "eglWaitSync",
        ]);
        assert!(
            mixed
                .select_consumer(
                    "EGL_ANDROID_native_fence_sync EGL_KHR_fence_sync",
                    "GL_OES_EGL_sync",
                    false,
                    false,
                )
                .is_none()
        );

        let core_missing_client = loader_with(&["eglCreateSync", "eglDestroySync", "eglWaitSync"]);
        assert!(
            core_missing_client
                .select_consumer("EGL_ANDROID_native_fence_sync", "", true, true)
                .is_none()
        );

        let incomplete_khr_complete_core = loader_with(&[
            "eglCreateSyncKHR",
            "eglCreateSync",
            "eglDestroySync",
            "eglClientWaitSync",
            "eglWaitSync",
        ]);
        let selected = incomplete_khr_complete_core
            .select_consumer(
                "EGL_ANDROID_native_fence_sync EGL_KHR_fence_sync EGL_KHR_wait_sync",
                "GL_OES_EGL_sync",
                true,
                true,
            )
            .expect("complete core fallback");
        assert_eq!(selected.sync_abi(), SyncAbi::Core15);
    }

    #[test]
    fn selection_obeys_extension_and_version_gates() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let all = loader_with(&[
            "eglCreateSyncKHR",
            "eglDestroySyncKHR",
            "eglClientWaitSyncKHR",
            "eglWaitSyncKHR",
            "eglCreateSync",
            "eglDestroySync",
            "eglClientWaitSync",
            "eglWaitSync",
        ]);
        let khr = all
            .select_consumer(
                "EGL_ANDROID_native_fence_sync EGL_KHR_fence_sync EGL_KHR_wait_sync",
                "GL_OES_EGL_sync",
                true,
                true,
            )
            .expect("KHR preferred");
        assert_eq!(khr.sync_abi(), SyncAbi::Khr);
        assert!(khr.capabilities().server_wait);

        let khr_without_server_extensions = all
            .select_consumer(
                "EGL_ANDROID_native_fence_sync EGL_KHR_fence_sync",
                "",
                true,
                true,
            )
            .expect("KHR base bundle remains selected");
        assert_eq!(khr_without_server_extensions.sync_abi(), SyncAbi::Khr);
        assert!(!khr_without_server_extensions.capabilities().server_wait);
        let handle = unsafe {
            khr_without_server_extensions
                .create(ptr::null_mut(), None)
                .expect("create")
        };
        assert_eq!(
            unsafe { khr_without_server_extensions.wait_server(&handle) },
            ServerWaitOutcome::Unsupported
        );
        unsafe {
            khr_without_server_extensions
                .destroy(handle)
                .expect("destroy")
        };

        let core_without_client_support = all
            .select_consumer("EGL_ANDROID_native_fence_sync", "", true, false)
            .expect("core 1.5 client-wait fallback");
        assert_eq!(core_without_client_support.sync_abi(), SyncAbi::Core15);
        assert!(!core_without_client_support.capabilities().server_wait);

        let core = all
            .select_consumer("EGL_ANDROID_native_fence_sync", "", true, true)
            .expect("core 1.5");
        assert_eq!(core.sync_abi(), SyncAbi::Core15);
        assert!(core.capabilities().server_wait);
        assert!(
            all.select_consumer("EGL_ANDROID_native_fence_sync", "", false, false)
                .is_none()
        );
        assert!(
            all.select_consumer("EGL_KHR_fence_sync", "GL_OES_EGL_sync", true, true)
                .is_none()
        );
    }

    #[test]
    fn server_wait_never_crosses_the_selected_abi() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        KHR_SERVER_CALLS.store(0, Ordering::SeqCst);
        CORE_SERVER_CALLS.store(0, Ordering::SeqCst);
        KHR_SERVER_WAIT_RESULT.store(7, Ordering::SeqCst);
        CORE_SERVER_WAIT_RESULT.store(1, Ordering::SeqCst);
        let loader = loader_with(&[
            "eglCreateSyncKHR",
            "eglDestroySyncKHR",
            "eglClientWaitSyncKHR",
            "eglWaitSyncKHR",
            "eglCreateSync",
            "eglDestroySync",
            "eglClientWaitSync",
            "eglWaitSync",
        ]);

        let khr = loader
            .select_consumer(
                "EGL_ANDROID_native_fence_sync EGL_KHR_fence_sync EGL_KHR_wait_sync",
                "GL_OES_EGL_sync",
                true,
                true,
            )
            .expect("KHR");
        let handle = unsafe { khr.create(ptr::null_mut(), None).expect("create") };
        assert_eq!(
            unsafe { khr.wait_server(&handle) },
            ServerWaitOutcome::Queued
        );
        assert_eq!(KHR_SERVER_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(CORE_SERVER_CALLS.load(Ordering::SeqCst), 0);
        unsafe { khr.destroy(handle).expect("destroy") };

        let core = loader
            .select_consumer("EGL_ANDROID_native_fence_sync", "", true, true)
            .expect("core");
        let handle = unsafe { core.create(ptr::null_mut(), None).expect("create") };
        assert_eq!(
            unsafe { core.wait_server(&handle) },
            ServerWaitOutcome::Queued
        );
        assert_eq!(KHR_SERVER_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(CORE_SERVER_CALLS.load(Ordering::SeqCst), 1);
        unsafe { core.destroy(handle).expect("destroy") };
    }

    #[test]
    fn server_wait_uses_each_abis_false_return_semantics() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let loader = loader_with(&[
            "eglCreateSyncKHR",
            "eglDestroySyncKHR",
            "eglClientWaitSyncKHR",
            "eglWaitSyncKHR",
            "eglCreateSync",
            "eglDestroySync",
            "eglClientWaitSync",
            "eglWaitSync",
        ]);
        KHR_SERVER_WAIT_RESULT.store(FALSE as Int, Ordering::SeqCst);
        let khr = loader
            .select_consumer(
                "EGL_ANDROID_native_fence_sync EGL_KHR_fence_sync EGL_KHR_wait_sync",
                "GL_OES_EGL_sync",
                true,
                true,
            )
            .expect("KHR");
        let handle = unsafe { khr.create(ptr::null_mut(), None).expect("create") };
        assert!(matches!(
            unsafe { khr.wait_server(&handle) },
            ServerWaitOutcome::Failed { .. }
        ));
        unsafe { khr.destroy(handle).expect("destroy") };

        CORE_SERVER_WAIT_RESULT.store(FALSE as usize, Ordering::SeqCst);
        let core = loader
            .select_consumer("EGL_ANDROID_native_fence_sync", "", true, true)
            .expect("core");
        let handle = unsafe { core.create(ptr::null_mut(), None).expect("create") };
        assert!(matches!(
            unsafe { core.wait_server(&handle) },
            ServerWaitOutcome::Failed { .. }
        ));
        unsafe { core.destroy(handle).expect("destroy") };
        KHR_SERVER_WAIT_RESULT.store(1, Ordering::SeqCst);
        CORE_SERVER_WAIT_RESULT.store(1, Ordering::SeqCst);
    }

    #[test]
    fn producer_requires_complete_khr_android_path() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let extensions = "EGL_ANDROID_native_fence_sync EGL_KHR_fence_sync";
        let core_only = loader_with(&[
            "eglCreateSync",
            "eglDestroySync",
            "eglDupNativeFenceFDANDROID",
        ]);
        assert!(core_only.select_producer(extensions).is_none());
        let no_duplicate = loader_with(&["eglCreateSyncKHR", "eglDestroySyncKHR"]);
        assert!(no_duplicate.select_producer(extensions).is_none());
        assert!(
            loader_with(&[
                "eglCreateSyncKHR",
                "eglDestroySyncKHR",
                "eglDupNativeFenceFDANDROID",
            ])
            .select_producer("EGL_ANDROID_native_fence_sync")
            .is_none()
        );
        let producer = khr_producer();
        assert!(producer.capabilities().producer_export);
        let handle = unsafe {
            producer
                .create_export_fence(ptr::null_mut())
                .expect("create")
        };
        assert_eq!(
            unsafe { producer.wait_server(&handle) },
            ServerWaitOutcome::Unsupported
        );
        unsafe { producer.destroy(handle).expect("destroy") };
    }

    #[test]
    fn import_transfers_fd_before_successful_create() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let (read, write) = pipe();
        let raw = read.as_raw_fd();
        CREATE_RESULT.store(1, Ordering::SeqCst);
        let functions = khr_consumer();
        let handle = unsafe {
            functions
                .import_sync_file(ptr::null_mut(), read)
                .expect("create")
        };
        assert_eq!(CREATE_FD.load(Ordering::SeqCst), raw);
        // Close the fake-EGL-owned fd and prove Rust did not close it first.
        assert_eq!(unsafe { libc::close(raw) }, 0);
        drop(write);
        unsafe { functions.destroy(handle).expect("destroy") };
    }

    #[test]
    fn import_does_not_reclaim_fd_when_create_fails() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let (read, write) = pipe();
        let raw = read.as_raw_fd();
        CREATE_RESULT.store(0, Ordering::SeqCst);
        let error = unsafe { khr_consumer().import_sync_file(ptr::null_mut(), read) }
            .expect_err("create should fail");
        assert!(matches!(error, SyncCreateError::Failed { .. }));
        assert_eq!(CREATE_FD.load(Ordering::SeqCst), raw);
        assert_eq!(unsafe { libc::close(raw) }, 0);
        drop(write);
        CREATE_RESULT.store(1, Ordering::SeqCst);
    }

    #[test]
    fn destroy_failure_returns_handle_for_retry() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        unsafe extern "system" fn fail(_display: Display, _sync: Sync) -> Boolean {
            0
        }
        let loader = unsafe {
            NativeFenceFunctions::load_with(|name| match name {
                "eglCreateSyncKHR" => create_khr as *const () as *const c_void,
                "eglDestroySyncKHR" => fail as *const () as *const c_void,
                "eglDupNativeFenceFDANDROID" => duplicate as *const () as *const c_void,
                _ => ptr::null(),
            })
        };
        let functions = loader
            .select_producer("EGL_ANDROID_native_fence_sync EGL_KHR_fence_sync")
            .expect("producer");
        let handle = unsafe {
            functions
                .create_export_fence(ptr::null_mut())
                .expect("create")
        };
        let error = unsafe { functions.destroy(handle) }.expect_err("destroy should fail");
        let retry = khr_producer();
        unsafe { retry.destroy(error.handle).expect("retry destroy") };
    }

    #[test]
    fn wait_outcomes_are_typed() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let functions = khr_consumer();
        let handle = unsafe { functions.create(ptr::null_mut(), None).expect("create") };

        KHR_SERVER_WAIT_RESULT.store(1, Ordering::SeqCst);
        assert_eq!(
            unsafe { functions.wait_server(&handle) },
            ServerWaitOutcome::Queued
        );
        CLIENT_WAIT_RESULT.store(TIMEOUT_EXPIRED, Ordering::SeqCst);
        assert_eq!(
            unsafe { functions.wait_client(&handle, Duration::from_millis(1)) },
            ClientWaitOutcome::Timeout
        );
        CLIENT_WAIT_RESULT.store(CONDITION_SATISFIED, Ordering::SeqCst);
        assert_eq!(
            unsafe { functions.wait_client(&handle, Duration::from_millis(1)) },
            ClientWaitOutcome::Satisfied
        );
        assert_eq!(CLIENT_WAIT_TIMEOUT.load(Ordering::SeqCst), 1_000_000);
        assert_eq!(
            unsafe { functions.wait_client(&handle, Duration::MAX) },
            ClientWaitOutcome::Satisfied
        );
        assert_eq!(CLIENT_WAIT_TIMEOUT.load(Ordering::SeqCst), FOREVER - 1);
        unsafe { functions.destroy(handle).expect("destroy") };
    }

    #[test]
    fn duplicated_native_fence_is_cloexec() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let functions = khr_producer();
        let handle = unsafe {
            functions
                .create_export_fence(ptr::null_mut())
                .expect("create")
        };
        let (read, write) = pipe();
        let duplicate_fd = unsafe { libc::dup(read.as_raw_fd()) };
        assert!(duplicate_fd >= 0);
        DUPLICATE_FD.store(duplicate_fd, Ordering::SeqCst);
        let fence = unsafe { functions.duplicate_fence(&handle).expect("duplicate") };
        let flags = unsafe { libc::fcntl(fence.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        drop((fence, read, write));
        unsafe { functions.destroy(handle).expect("destroy") };
    }

    #[test]
    fn poll_is_finite_and_observes_signal() {
        let _lock = TEST_LOCK.lock().expect("test lock");
        let (read, write) = pipe();
        assert!(matches!(
            poll_sync_file(&read, Duration::from_millis(1)),
            SyncFilePollOutcome::Timeout
        ));
        let byte = [1_u8];
        assert_eq!(
            unsafe { libc::write(write.as_raw_fd(), byte.as_ptr().cast(), 1) },
            1
        );
        assert!(matches!(
            poll_sync_file(&read, Duration::from_millis(20)),
            SyncFilePollOutcome::Signaled
        ));
    }

    fn pipe() -> (OwnedFd, OwnedFd) {
        let mut fds = [-1; 2];
        assert_eq!(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) }, 0);
        // SAFETY: pipe2 returned fresh descriptors.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }
}
