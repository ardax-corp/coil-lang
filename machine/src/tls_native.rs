//! Dloaded `coil_tls_*` ABI used by [`StreamKind::Tls`](crate::memory::StreamKind).
//!
//! Leftover HostInvoke ([`crate::tls`]) resolves these symbols, attaches a
//! [`NativeTlsSession`], and parks WouldBlock on
//! [`crate::io::reactor_wait_fd_no_help`]. Handshake then continues on
//! `stream_read` / `stream_write` — never call enable again, never free a
//! WouldBlock session. `coil_tls_disable` is close_notify; `coil_tls_free`
//! is the destructor.

use std::cell::Cell;
use std::ffi::{CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::sync::{Arc, Mutex, OnceLock};

use libloading::Library;

use crate::ffi::{FfiError, resolve_library};
use crate::io::IoErrorTag;
use crate::io_handle::NativeHandle;
use crate::memory::{Heap, ObjStream, StreamKind};

/// `err_out` NULL / empty means success (`tls.h`).
pub const ABI_OK: i32 = -1;

type ClientEnableFn = unsafe extern "C" fn(
    i64,
    *const c_char,
    i64,
    *const c_char,
    *const c_char,
    i64,
    *const c_char,
    *mut *const c_char,
) -> i64;
type ServerEnableFn = unsafe extern "C" fn(
    i64,
    *const c_char,
    *const c_char,
    i64,
    *const c_char,
    *const c_char,
    *mut *const c_char,
) -> i64;
type ReadFn = unsafe extern "C" fn(i64, i64, *mut u8, i64, *mut *const c_char) -> i64;
type WriteFn = unsafe extern "C" fn(i64, i64, *const u8, i64, *mut *const c_char) -> i64;
type AlpnFn = unsafe extern "C" fn(i64, *mut u8, i64) -> i64;
type DisableFn = unsafe extern "C" fn(i64, i64, *mut *const c_char);
type FreeFn = unsafe extern "C" fn(i64);

/// Resolved `coil_tls_*` function pointers plus the owning library.
pub struct TlsNativeAbi {
    lib: Arc<Library>,
    client_enable: ClientEnableFn,
    server_enable: ServerEnableFn,
    read: ReadFn,
    write: WriteFn,
    alpn: AlpnFn,
    disable: DisableFn,
    free: FreeFn,
}

impl TlsNativeAbi {
    /// Bind the seven ABI symbols from an already-loaded `libtls`.
    pub fn from_library(lib: Arc<Library>) -> Result<Arc<Self>, FfiError> {
        unsafe {
            Ok(Arc::new(Self {
                client_enable: load_sym(&lib, "coil_tls_client_enable")?,
                server_enable: load_sym(&lib, "coil_tls_server_enable")?,
                read: load_sym(&lib, "coil_tls_read")?,
                write: load_sym(&lib, "coil_tls_write")?,
                alpn: load_sym(&lib, "coil_tls_alpn")?,
                disable: load_sym(&lib, "coil_tls_disable")?,
                free: load_sym(&lib, "coil_tls_free")?,
                lib,
            }))
        }
    }

    /// Load `libtls` from an absolute path.
    pub fn load_path(path: &Path) -> Result<Arc<Self>, FfiError> {
        let lib = unsafe { Library::new(path) }.map_err(|e| FfiError::LibraryNotFound {
            name: path.display().to_string(),
            tried: vec![path.display().to_string()],
            detail: e.to_string(),
        })?;
        Self::from_library(Arc::new(lib))
    }

    /// Resolve `dload("tls")` against `base_dir` / `search_paths`.
    pub fn resolve(
        base_dir: Option<&Path>,
        search_paths: &[PathBuf],
    ) -> Result<Arc<Self>, FfiError> {
        let lib = resolve_library("tls", base_dir, search_paths)?;
        Self::from_library(lib)
    }

    pub fn library(&self) -> &Library {
        &self.lib
    }

    /// Create a client session in the `.so`. `timeout_ms <= 0` means no deadline.
    ///
    /// One handshake step per call. WouldBlock still yields a session to attach;
    /// the next step is read/write, not another enable.
    pub fn client_enable(
        self: &Arc<Self>,
        fd: i64,
        host: &str,
        verify: bool,
        ca_pem: Option<&str>,
        ca_path: Option<&str>,
        timeout_ms: i64,
        alpn: &str,
    ) -> NativeEnable {
        let host = match c_string(host) {
            Ok(s) => s,
            Err(e) => return NativeEnable::Failed(e),
        };
        let ca_pem = match optional_c_string(ca_pem) {
            Ok(s) => s,
            Err(e) => return NativeEnable::Failed(e),
        };
        let ca_path = match optional_c_string(ca_path) {
            Ok(s) => s,
            Err(e) => return NativeEnable::Failed(e),
        };
        let alpn = match c_string(alpn) {
            Ok(s) => s,
            Err(e) => return NativeEnable::Failed(e),
        };
        let mut err: *const c_char = ptr::null();
        let ptr = unsafe {
            (self.client_enable)(
                fd,
                host.as_ptr(),
                i64::from(verify),
                opt_ptr(&ca_pem),
                opt_ptr(&ca_path),
                timeout_ms,
                alpn.as_ptr(),
                &mut err,
            )
        };
        session_from_enable(self.clone(), ptr, err)
    }

    /// Create a server session in the `.so`. `timeout_ms <= 0` means no deadline.
    pub fn server_enable(
        self: &Arc<Self>,
        fd: i64,
        cert_pem: &str,
        key_pem: &str,
        timeout_ms: i64,
        client_ca_pem: &str,
        alpn: &str,
    ) -> NativeEnable {
        let cert = match c_string(cert_pem) {
            Ok(s) => s,
            Err(e) => return NativeEnable::Failed(e),
        };
        let key = match c_string(key_pem) {
            Ok(s) => s,
            Err(e) => return NativeEnable::Failed(e),
        };
        let client_ca = match c_string(client_ca_pem) {
            Ok(s) => s,
            Err(e) => return NativeEnable::Failed(e),
        };
        let alpn = match c_string(alpn) {
            Ok(s) => s,
            Err(e) => return NativeEnable::Failed(e),
        };
        let mut err: *const c_char = ptr::null();
        let ptr = unsafe {
            (self.server_enable)(
                fd,
                cert.as_ptr(),
                key.as_ptr(),
                timeout_ms,
                client_ca.as_ptr(),
                alpn.as_ptr(),
                &mut err,
            )
        };
        session_from_enable(self.clone(), ptr, err)
    }
}

unsafe fn load_sym<T: Copy>(lib: &Library, name: &str) -> Result<T, FfiError> {
    let s: libloading::Symbol<T> = unsafe {
        lib.get(name.as_bytes())
            .map_err(|_| FfiError::SymbolNotFound {
                name: name.to_string(),
            })?
    };
    Ok(*s)
}

fn c_string(s: &str) -> Result<CString, IoErrorTag> {
    CString::new(s).map_err(|_| IoErrorTag::InvalidInput)
}

fn optional_c_string(s: Option<&str>) -> Result<Option<CString>, IoErrorTag> {
    match s {
        None => Ok(None),
        Some(s) => Ok(Some(c_string(s)?)),
    }
}

fn opt_ptr(s: &Option<CString>) -> *const c_char {
    s.as_ref().map(|c| c.as_ptr()).unwrap_or(ptr::null())
}

/// Outcome of `coil_tls_*_enable`. `Result<Session, IoError>` cannot carry a
/// session on WouldBlock; this shape can.
#[must_use]
pub enum NativeEnable {
    Ready(NativeTlsSession),
    WouldBlock(NativeTlsSession),
    Failed(IoErrorTag),
}

fn tag_from_err_out(err: *const c_char) -> Option<IoErrorTag> {
    if err.is_null() {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(err) }
        .to_str()
        .unwrap_or("");
    if name.is_empty() {
        None
    } else {
        Some(IoErrorTag::from_abi_name(name))
    }
}

fn session_from_enable(abi: Arc<TlsNativeAbi>, ptr: i64, err: *const c_char) -> NativeEnable {
    let keep = |abi: Arc<TlsNativeAbi>| {
        NonNull::new(ptr as *mut c_void).map(|p| NativeTlsSession {
            ptr: Some(p),
            abi,
            wants_write: Cell::new(false),
        })
    };
    match tag_from_err_out(err) {
        None => keep(abi)
            .map(NativeEnable::Ready)
            .unwrap_or(NativeEnable::Failed(IoErrorTag::Other)),
        Some(IoErrorTag::WouldBlock) => keep(abi)
            .map(NativeEnable::WouldBlock)
            .unwrap_or(NativeEnable::Failed(IoErrorTag::WouldBlock)),
        Some(tag) => {
            if ptr != 0 {
                unsafe { (abi.free)(ptr) };
            }
            NativeEnable::Failed(tag)
        }
    }
}

/// Opaque session living in dloaded `libtls`.
pub struct NativeTlsSession {
    ptr: Option<NonNull<c_void>>,
    abi: Arc<TlsNativeAbi>,
    /// Last WouldBlock was a write (or client enable). Used when the `.so`
    /// does not export `wants_write` so leftover park picks the right interest.
    wants_write: Cell<bool>,
}

unsafe impl Send for NativeTlsSession {}
unsafe impl Sync for NativeTlsSession {}

impl NativeTlsSession {
    fn raw_i64(&self) -> i64 {
        self.ptr.map(|p| p.as_ptr() as i64).unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn test_ptr(&self) -> *mut c_void {
        self.ptr.map(NonNull::as_ptr).unwrap_or(ptr::null_mut())
    }

    fn take_i64(&mut self) -> i64 {
        self.ptr.take().map(|p| p.as_ptr() as i64).unwrap_or(0)
    }

    /// Handshake/IO park interest: true → wait writable (ClientHello / flush).
    pub fn set_wants_write(&self, wants: bool) {
        self.wants_write.set(wants);
    }

    pub fn wants_write(&self) -> bool {
        self.wants_write.get()
    }

    pub fn read(&self, fd: i64, buf: &mut [u8]) -> Result<Option<usize>, IoErrorTag> {
        let ptr = self.raw_i64();
        if ptr == 0 {
            return Err(IoErrorTag::AlreadyClosed);
        }
        let mut err: *const c_char = ptr::null();
        let n = unsafe { (self.abi.read)(ptr, fd, buf.as_mut_ptr(), buf.len() as i64, &mut err) };
        if let Some(tag) = tag_from_err_out(err) {
            if tag == IoErrorTag::WouldBlock {
                self.wants_write.set(false);
            }
            return Err(tag);
        }
        if n < 0 {
            return Err(IoErrorTag::Other);
        }
        if n == 0 {
            // Empty-buffer probe: success means handshake can proceed / is done.
            if buf.is_empty() {
                Ok(Some(0))
            } else {
                Ok(None)
            }
        } else {
            Ok(Some(n as usize))
        }
    }

    pub fn write(&self, fd: i64, buf: &[u8]) -> Result<usize, IoErrorTag> {
        let ptr = self.raw_i64();
        if ptr == 0 {
            return Err(IoErrorTag::AlreadyClosed);
        }
        let mut err: *const c_char = ptr::null();
        let n = unsafe { (self.abi.write)(ptr, fd, buf.as_ptr(), buf.len() as i64, &mut err) };
        if let Some(tag) = tag_from_err_out(err) {
            if tag == IoErrorTag::WouldBlock {
                self.wants_write.set(true);
            }
            return Err(tag);
        }
        if n < 0 {
            return Err(IoErrorTag::Other);
        }
        Ok(n as usize)
    }

    pub fn alpn_protocol(&self) -> String {
        let ptr = self.raw_i64();
        if ptr == 0 {
            return String::new();
        }
        let n = unsafe { (self.abi.alpn)(ptr, ptr::null_mut(), 0) };
        if n <= 0 {
            return String::new();
        }
        let mut buf = vec![0u8; n as usize];
        let n = unsafe { (self.abi.alpn)(ptr, buf.as_mut_ptr(), buf.len() as i64) };
        if n <= 0 {
            return String::new();
        }
        String::from_utf8_lossy(&buf[..n as usize]).into_owned()
    }

    /// One non-blocking handshake pump. `Ok` means rustls is past handshake.
    pub fn handshake_step(&self, fd: i64) -> Result<(), IoErrorTag> {
        match self.write(fd, &[]) {
            Ok(_) => Ok(()),
            Err(IoErrorTag::WouldBlock) => match self.read(fd, &mut []) {
                Ok(_) => Ok(()),
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        }
    }

    pub fn disable(&self, fd: i64) -> Result<(), IoErrorTag> {
        let ptr = self.raw_i64();
        if ptr == 0 {
            return Ok(());
        }
        let mut err: *const c_char = ptr::null();
        unsafe { (self.abi.disable)(ptr, fd, &mut err) };
        if let Some(tag) = tag_from_err_out(err) {
            Err(tag)
        } else {
            Ok(())
        }
    }
}

impl Drop for NativeTlsSession {
    fn drop(&mut self) {
        let ptr = self.take_i64();
        if ptr != 0 {
            unsafe { (self.abi.free)(ptr) };
        }
    }
}

/// TLS session stored on [`ObjStream`]: a native `.so` pointer.
pub enum TlsSessionSlot {
    /// Session owned by dloaded `libtls`.
    Native(NativeTlsSession),
}

impl TlsSessionSlot {
    pub fn has_buffered_plaintext(&self) -> bool {
        match self {
            Self::Native(_) => false,
        }
    }

    pub fn wants_write(&self) -> bool {
        match self {
            Self::Native(n) => n.wants_write(),
        }
    }

    pub fn handshake_step(&self, fd: i64) -> Result<(), IoErrorTag> {
        match self {
            Self::Native(n) => n.handshake_step(fd),
        }
    }

    pub fn alpn_protocol(&self) -> String {
        match self {
            Self::Native(n) => n.alpn_protocol(),
        }
    }
}

/// Non-blocking app read for a Tls-kind stream.
pub fn slot_read(
    handle: &mut NativeHandle,
    slot: &mut TlsSessionSlot,
    buf: &mut [u8],
) -> Result<Option<usize>, IoErrorTag> {
    match slot {
        TlsSessionSlot::Native(n) => n.read(handle.tls_abi_fd(), buf),
    }
}

/// Non-blocking app write for a Tls-kind stream.
pub fn slot_write(
    handle: &mut NativeHandle,
    slot: &mut TlsSessionSlot,
    buf: &[u8],
) -> Result<usize, IoErrorTag> {
    match slot {
        TlsSessionSlot::Native(n) => n.write(handle.tls_abi_fd(), buf),
    }
}

/// Free the native session (`coil_tls_free`). close_notify is leftover `tls_*_disable`.
///
/// Stream close / GC must not call `coil_tls_disable`: coil-tls currently frees
/// inside disable (`tls.h`), so a following Drop `free` would double-free.
pub fn drop_slot(_handle: Option<&mut NativeHandle>, slot: TlsSessionSlot) {
    drop(slot);
}

/// Attach a native session to a TCP stream (`kind` stays Tcp on failure).
pub fn attach_native(
    heap: &mut Heap,
    stream: common::Value,
    session: NativeTlsSession,
) -> Result<(), IoErrorTag> {
    crate::io::with_stream_mut(heap, stream, |s: &mut ObjStream| {
        if s.closed || s.handle.is_none() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        if s.kind != StreamKind::Tcp || s.tls.is_some() {
            return Err(IoErrorTag::InvalidInput);
        }
        s.kind = StreamKind::Tls;
        s.tls = Some(TlsSessionSlot::Native(session));
        Ok(())
    })?
}

/// Attach a [`NativeEnable`] outcome. Ready and WouldBlock both set `kind = Tls`.
/// WouldBlock returns `Err(WouldBlock)` after attach so the caller parks, then
/// continues on read/write. Failed leaves `kind = Tcp`.
pub fn attach_enable_outcome(
    heap: &mut Heap,
    stream: common::Value,
    outcome: NativeEnable,
) -> Result<(), IoErrorTag> {
    match outcome {
        NativeEnable::Ready(session) => attach_native(heap, stream, session),
        NativeEnable::WouldBlock(session) => {
            attach_native(heap, stream, session)?;
            Err(IoErrorTag::WouldBlock)
        }
        NativeEnable::Failed(err) => Err(err),
    }
}

/// Remember a `dload("tls")` so later HostInvoke enable can prefer the `.so`.
pub fn note_loaded_library(name: &str, lib: Arc<Library>) {
    if !crate::ffi::library_stem(name).eq_ignore_ascii_case("tls") {
        return;
    }
    if let Ok(abi) = TlsNativeAbi::from_library(lib) {
        set_preferred(abi);
    }
}

/// Remember FFI search paths and try to resolve `libtls` (no-op if missing).
pub fn note_search_paths(base_dir: Option<&Path>, search_paths: &[PathBuf]) {
    *search_paths_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner()) =
        (base_dir.map(Path::to_path_buf), search_paths.to_vec());
    if preferred().is_some() {
        return;
    }
    if let Ok(abi) = TlsNativeAbi::resolve(base_dir, search_paths) {
        set_preferred(abi);
    }
}

/// Process-wide ABI after `dload("tls")` / search-path resolve.
pub fn preferred() -> Option<Arc<TlsNativeAbi>> {
    preferred_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// `preferred()`, or a resolve against the last [`note_search_paths`] roots.
pub fn resolve_preferred() -> Option<Arc<TlsNativeAbi>> {
    if let Some(abi) = preferred() {
        return Some(abi);
    }
    let (base, paths) = search_paths_slot()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Ok(abi) = TlsNativeAbi::resolve(base.as_deref(), &paths) {
        set_preferred(abi.clone());
        Some(abi)
    } else {
        None
    }
}

fn set_preferred(abi: Arc<TlsNativeAbi>) {
    *preferred_slot().lock().unwrap_or_else(|e| e.into_inner()) = Some(abi);
}

fn preferred_slot() -> &'static Mutex<Option<Arc<TlsNativeAbi>>> {
    static SLOT: OnceLock<Mutex<Option<Arc<TlsNativeAbi>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn search_paths_slot() -> &'static Mutex<(Option<PathBuf>, Vec<PathBuf>)> {
    static SLOT: OnceLock<Mutex<(Option<PathBuf>, Vec<PathBuf>)>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new((None, Vec::new())))
}

#[cfg(test)]
pub(crate) fn install_preferred(abi: Arc<TlsNativeAbi>) {
    set_preferred(abi);
}

#[cfg(test)]
pub(crate) fn reset_preferred() {
    *preferred_slot().lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::platform_shared_lib_filename;
    use crate::io::{
        alloc_stream, reactor_wait_fd_no_help, stream_close, stream_read, stream_write,
        with_stream_mut,
    };
    use crate::io_reactor::Interest;
    use crate::memory::{Heap, Member, ObjArray, Object};
    use common::Value;
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("machine crate parent")
            .to_path_buf()
    }

    fn stub_src() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/tls_abi_stub.c")
    }

    fn stub_lib_path() -> PathBuf {
        workspace_root()
            .join("target")
            .join("tls_abi_stub")
            .join(platform_shared_lib_filename("tls"))
    }

    fn ensure_stub_built() -> Option<PathBuf> {
        static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
        PATH.get_or_init(|| {
            let src = stub_src();
            let dest = stub_lib_path();
            if let Some(parent) = dest.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let needs_build = match (src.metadata(), dest.metadata()) {
                (Ok(s), Ok(d)) => s.modified().ok() > d.modified().ok(),
                (Ok(_), Err(_)) => true,
                _ => false,
            };
            if !needs_build && dest.exists() {
                return Some(dest);
            }
            let mut cmd = std::process::Command::new("cc");
            #[cfg(target_os = "macos")]
            {
                cmd.arg("-dynamiclib");
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                cmd.arg("-shared").arg("-fPIC");
            }
            #[cfg(target_os = "windows")]
            {
                cmd.arg("-shared");
            }
            let status = cmd.arg("-O2").arg("-o").arg(&dest).arg(&src).status();
            match status {
                Ok(s) if s.success() && dest.exists() => Some(dest),
                _ => None,
            }
        })
        .clone()
    }

    fn load_stub() -> Option<Arc<TlsNativeAbi>> {
        let path = ensure_stub_built()?;
        TlsNativeAbi::load_path(&path).ok()
    }

    fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _) = listener.accept().unwrap();
        client.set_nonblocking(true).ok();
        server.set_nonblocking(true).ok();
        (client, server)
    }

    fn make_byte_array(heap: &mut Heap, bytes: &[u8]) -> Value {
        let elements: Vec<Value> = bytes.iter().map(|&b| Value::from(b as i64)).collect();
        let (obj, _) = heap.alloc(ObjArray { elements }, Object::Array);
        Value::from(obj.addr())
    }

    fn stub_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    unsafe fn stub_sym<T: Copy>(abi: &TlsNativeAbi, name: &[u8]) -> T {
        let s: libloading::Symbol<T> = unsafe { abi.library().get(name).expect("stub hook") };
        *s
    }

    unsafe fn stub_set_would_block_reads(abi: &TlsNativeAbi, session: *mut c_void, n: i32) {
        let f: unsafe extern "C" fn(i64, i32) =
            unsafe { stub_sym(abi, b"coil_tls_stub_set_would_block_reads\0") };
        unsafe { f(session as i64, n) };
    }

    unsafe fn stub_read_calls(abi: &TlsNativeAbi, session: *mut c_void) -> i32 {
        let f: unsafe extern "C" fn(i64) -> i32 =
            unsafe { stub_sym(abi, b"coil_tls_stub_read_calls\0") };
        unsafe { f(session as i64) }
    }

    unsafe fn stub_write_calls(abi: &TlsNativeAbi, session: *mut c_void) -> i32 {
        let f: unsafe extern "C" fn(i64) -> i32 =
            unsafe { stub_sym(abi, b"coil_tls_stub_write_calls\0") };
        unsafe { f(session as i64) }
    }

    unsafe fn stub_disable_calls(abi: &TlsNativeAbi, session: *mut c_void) -> i32 {
        let f: unsafe extern "C" fn(i64) -> i32 =
            unsafe { stub_sym(abi, b"coil_tls_stub_disable_calls\0") };
        unsafe { f(session as i64) }
    }

    unsafe fn stub_set_next_enable_err(abi: &TlsNativeAbi, err: i32) {
        let f: unsafe extern "C" fn(i32) =
            unsafe { stub_sym(abi, b"coil_tls_stub_set_next_enable_err\0") };
        unsafe { f(err) };
    }

    unsafe fn stub_live_sessions(abi: &TlsNativeAbi) -> i32 {
        let f: unsafe extern "C" fn() -> i32 =
            unsafe { stub_sym(abi, b"coil_tls_stub_live_sessions\0") };
        unsafe { f() }
    }

    unsafe fn stub_enable_calls(abi: &TlsNativeAbi) -> i32 {
        let f: unsafe extern "C" fn() -> i32 =
            unsafe { stub_sym(abi, b"coil_tls_stub_enable_calls\0") };
        unsafe { f() }
    }

    unsafe fn stub_free_calls(abi: &TlsNativeAbi) -> i32 {
        let f: unsafe extern "C" fn() -> i32 =
            unsafe { stub_sym(abi, b"coil_tls_stub_free_calls\0") };
        unsafe { f() }
    }

    fn expect_ready(outcome: NativeEnable) -> NativeTlsSession {
        match outcome {
            NativeEnable::Ready(s) => s,
            NativeEnable::WouldBlock(_) => panic!("expected Ready, got WouldBlock"),
            NativeEnable::Failed(e) => panic!("expected Ready, got Failed({e:?})"),
        }
    }

    fn attach_stub_stream(heap: &mut Heap, sock: TcpStream, abi: &Arc<TlsNativeAbi>) -> Value {
        let fd = {
            #[cfg(unix)]
            {
                use std::os::fd::AsRawFd;
                sock.as_raw_fd() as i64
            }
            #[cfg(windows)]
            {
                use std::os::windows::io::AsRawSocket;
                sock.as_raw_socket() as i64
            }
        };
        let stream = alloc_stream(heap, NativeHandle::Tcp(sock), StreamKind::Tcp).expect("alloc");
        let session = expect_ready(abi.client_enable(fd, "localhost", false, None, None, 0, ""));
        attach_native(heap, stream, session).expect("attach");
        stream
    }

    fn native_ptr(heap: &mut Heap, stream: Value) -> *mut c_void {
        with_stream_mut(heap, stream, |s| match s.tls.as_ref() {
            Some(TlsSessionSlot::Native(n)) => n.test_ptr(),
            _ => ptr::null_mut(),
        })
        .expect("stream")
    }

    #[test]
    fn stub_dload_read_write_close_hit_coil_tls_symbols() {
        let Some(abi) = load_stub() else {
            eprintln!("skip: cc could not build tls ABI stub");
            return;
        };
        let _guard = stub_lock();
        unsafe { stub_set_next_enable_err(&abi, ABI_OK) };
        let (client, mut server) = tcp_pair();
        let mut heap = Heap::default();
        let stream = attach_stub_stream(&mut heap, client, &abi);
        let ptr = native_ptr(&mut heap, stream);
        assert!(!ptr.is_null());

        let buf = make_byte_array(&mut heap, &[0; 16]);
        let n = stream_read(&mut heap, stream, buf).expect("read");
        assert_eq!(n, Some(5));
        assert!(unsafe { stub_read_calls(&abi, ptr) } >= 1);

        let out = make_byte_array(&mut heap, b"abc");
        let wrote = crate::io::stream_write(&mut heap, stream, out).expect("write");
        assert_eq!(wrote, 3);
        assert!(unsafe { stub_write_calls(&abi, ptr) } >= 1);

        stream_close(&mut heap, stream).expect("close");
        assert!(with_stream_mut(&mut heap, stream, |s| s.closed).unwrap());
        assert!(with_stream_mut(&mut heap, stream, |s| s.tls.is_none()).unwrap());
        let _ = server.write_all(b"x");
    }

    #[test]
    fn stub_would_block_parks_via_reactor_wait_fd_no_help() {
        let Some(abi) = load_stub() else {
            eprintln!("skip: cc could not build tls ABI stub");
            return;
        };
        let _guard = stub_lock();
        unsafe { stub_set_next_enable_err(&abi, ABI_OK) };
        let (client, mut server) = tcp_pair();
        let wait = crate::io_handle::WaitHandle::from_tcp(&client);
        let mut heap = Heap::default();
        let stream = attach_stub_stream(&mut heap, client, &abi);
        let ptr = native_ptr(&mut heap, stream);
        unsafe { stub_set_would_block_reads(&abi, ptr, 1) };

        let buf = make_byte_array(&mut heap, &[0; 16]);
        let err = stream_read(&mut heap, stream, buf).unwrap_err();
        assert_eq!(err, IoErrorTag::WouldBlock);
        assert!(unsafe { stub_read_calls(&abi, ptr) } >= 1);

        let parked =
            reactor_wait_fd_no_help(wait, Interest::Readable, Some(Duration::from_millis(25)));
        assert_eq!(parked, Err(IoErrorTag::TimedOut));

        server.write_all(b"x").ok();
        reactor_wait_fd_no_help(wait, Interest::Readable, Some(Duration::from_millis(200)))
            .expect("peer byte should wake readable");

        let n = stream_read(&mut heap, stream, buf).expect("retry read");
        assert_eq!(n, Some(5));
        stream_close(&mut heap, stream).ok();
    }

    #[test]
    fn stub_enable_failure_does_not_attach() {
        let Some(abi) = load_stub() else {
            eprintln!("skip: cc could not build tls ABI stub");
            return;
        };
        let _guard = stub_lock();
        unsafe { stub_set_next_enable_err(&abi, ABI_OK) };
        let (client, server) = tcp_pair();
        let mut heap = Heap::default();
        let stream =
            alloc_stream(&mut heap, NativeHandle::Tcp(client), StreamKind::Tcp).expect("alloc");
        // Failed attach: kind stays Tcp, no session (drop the session without attach).
        let fd = with_stream_mut(&mut heap, stream, |s| {
            s.handle.as_ref().map(|h| h.tls_abi_fd()).unwrap_or(-1)
        })
        .unwrap();
        let session = expect_ready(abi.client_enable(fd, "localhost", false, None, None, 0, ""));
        drop(session);
        assert_eq!(
            with_stream_mut(&mut heap, stream, |s| s.kind).unwrap(),
            StreamKind::Tcp
        );
        assert!(with_stream_mut(&mut heap, stream, |s| s.tls.is_none()).unwrap());
        drop(server);
    }

    #[test]
    fn stub_alpn_and_write_would_block() {
        let Some(abi) = load_stub() else {
            eprintln!("skip: cc could not build tls ABI stub");
            return;
        };
        let _guard = stub_lock();
        unsafe { stub_set_next_enable_err(&abi, ABI_OK) };
        let (client, server) = tcp_pair();
        let mut heap = Heap::default();
        let stream = attach_stub_stream(&mut heap, client, &abi);
        let proto = with_stream_mut(&mut heap, stream, |s| {
            s.tls
                .as_ref()
                .map(|t| t.alpn_protocol())
                .unwrap_or_default()
        })
        .unwrap();
        assert_eq!(proto, "h2");

        let ptr = native_ptr(&mut heap, stream);
        type SetWrites = unsafe extern "C" fn(i64, i32);
        let set_w: libloading::Symbol<SetWrites> = unsafe {
            abi.library()
                .get(b"coil_tls_stub_set_would_block_writes\0")
                .expect("hook")
        };
        unsafe { set_w(ptr as i64, 1) };
        let out = make_byte_array(&mut heap, b"xyz");
        let err = stream_write(&mut heap, stream, out).unwrap_err();
        assert_eq!(err, IoErrorTag::WouldBlock);
        stream_close(&mut heap, stream).ok();
        drop(server);
    }

    #[test]
    fn resolve_tls_from_stub_search_path() {
        let Some(path) = ensure_stub_built() else {
            eprintln!("skip: cc could not build tls ABI stub");
            return;
        };
        let _guard = stub_lock();
        let dir = path.parent().unwrap();
        let abi = TlsNativeAbi::resolve(None, &[dir.to_path_buf()]).expect("resolve tls");
        unsafe { stub_set_next_enable_err(&abi, ABI_OK) };
        let (client, server) = tcp_pair();
        #[cfg(unix)]
        let fd = {
            use std::os::fd::AsRawFd;
            client.as_raw_fd() as i64
        };
        #[cfg(windows)]
        let fd = {
            use std::os::windows::io::AsRawSocket;
            client.as_raw_socket() as i64
        };
        let session = expect_ready(abi.client_enable(fd, "localhost", false, None, None, 0, "h2"));
        drop(session);
        drop(client);
        drop(server);
    }

    #[test]
    fn stub_enable_would_block_attaches_and_keeps_session() {
        let Some(abi) = load_stub() else {
            eprintln!("skip: cc could not build tls ABI stub");
            return;
        };
        let _guard = stub_lock();
        unsafe { stub_set_next_enable_err(&abi, ABI_OK) };
        let (client, server) = tcp_pair();
        let mut heap = Heap::default();
        let stream =
            alloc_stream(&mut heap, NativeHandle::Tcp(client), StreamKind::Tcp).expect("alloc");
        let fd = with_stream_mut(&mut heap, stream, |s| {
            s.handle.as_ref().map(|h| h.tls_abi_fd()).unwrap_or(-1)
        })
        .unwrap();

        let live0 = unsafe { stub_live_sessions(&abi) };
        let enable0 = unsafe { stub_enable_calls(&abi) };
        let free0 = unsafe { stub_free_calls(&abi) };
        unsafe { stub_set_next_enable_err(&abi, IoErrorTag::WouldBlock as i32) };

        let outcome = abi.client_enable(fd, "localhost", false, None, None, 0, "");
        assert!(matches!(outcome, NativeEnable::WouldBlock(_)));
        assert_eq!(unsafe { stub_live_sessions(&abi) }, live0 + 1);
        assert_eq!(unsafe { stub_enable_calls(&abi) }, enable0 + 1);
        assert_eq!(unsafe { stub_free_calls(&abi) }, free0);

        let err = attach_enable_outcome(&mut heap, stream, outcome).unwrap_err();
        assert_eq!(err, IoErrorTag::WouldBlock);
        assert_eq!(
            with_stream_mut(&mut heap, stream, |s| s.kind).unwrap(),
            StreamKind::Tls
        );
        assert!(with_stream_mut(&mut heap, stream, |s| s.tls.is_some()).unwrap());
        assert_eq!(unsafe { stub_free_calls(&abi) }, free0);

        let ptr = native_ptr(&mut heap, stream);
        assert!(!ptr.is_null());
        let buf = make_byte_array(&mut heap, &[0; 16]);
        let n = stream_read(&mut heap, stream, buf).expect("read after WouldBlock enable");
        assert_eq!(n, Some(5));
        assert!(unsafe { stub_read_calls(&abi, ptr) } >= 1);
        assert_eq!(unsafe { stub_enable_calls(&abi) }, enable0 + 1);

        let out = make_byte_array(&mut heap, b"xy");
        let wrote = stream_write(&mut heap, stream, out).expect("write after WouldBlock enable");
        assert_eq!(wrote, 2);
        assert!(unsafe { stub_write_calls(&abi, ptr) } >= 1);
        assert_eq!(unsafe { stub_enable_calls(&abi) }, enable0 + 1);

        stream_close(&mut heap, stream).ok();
        assert_eq!(unsafe { stub_live_sessions(&abi) }, live0);
        assert_eq!(unsafe { stub_free_calls(&abi) }, free0 + 1);
        drop(server);
    }

    #[test]
    fn stub_enable_handshake_error_frees_and_does_not_attach() {
        let Some(abi) = load_stub() else {
            eprintln!("skip: cc could not build tls ABI stub");
            return;
        };
        let _guard = stub_lock();
        unsafe { stub_set_next_enable_err(&abi, ABI_OK) };
        let (client, server) = tcp_pair();
        let mut heap = Heap::default();
        let stream =
            alloc_stream(&mut heap, NativeHandle::Tcp(client), StreamKind::Tcp).expect("alloc");
        let fd = with_stream_mut(&mut heap, stream, |s| {
            s.handle.as_ref().map(|h| h.tls_abi_fd()).unwrap_or(-1)
        })
        .unwrap();

        let live0 = unsafe { stub_live_sessions(&abi) };
        let free0 = unsafe { stub_free_calls(&abi) };
        unsafe { stub_set_next_enable_err(&abi, IoErrorTag::Handshake as i32) };

        let outcome = abi.client_enable(fd, "localhost", false, None, None, 0, "");
        assert!(matches!(
            outcome,
            NativeEnable::Failed(IoErrorTag::Handshake)
        ));
        assert_eq!(unsafe { stub_live_sessions(&abi) }, live0);
        assert_eq!(unsafe { stub_free_calls(&abi) }, free0);

        let err = attach_enable_outcome(&mut heap, stream, outcome).unwrap_err();
        assert_eq!(err, IoErrorTag::Handshake);
        assert_eq!(
            with_stream_mut(&mut heap, stream, |s| s.kind).unwrap(),
            StreamKind::Tcp
        );
        assert!(with_stream_mut(&mut heap, stream, |s| s.tls.is_none()).unwrap());
        assert_eq!(unsafe { stub_free_calls(&abi) }, free0);
        drop(server);
    }

    #[test]
    fn stub_disable_is_close_notify_free_is_destructor() {
        let Some(abi) = load_stub() else {
            eprintln!("skip: cc could not build tls ABI stub");
            return;
        };
        let _guard = stub_lock();
        unsafe { stub_set_next_enable_err(&abi, ABI_OK) };
        let session = expect_ready(abi.client_enable(-1, "localhost", false, None, None, 0, ""));
        let ptr = session.test_ptr();
        let live = unsafe { stub_live_sessions(&abi) };
        let frees = unsafe { stub_free_calls(&abi) };
        session.disable(-1).expect("close_notify");
        assert!(unsafe { stub_disable_calls(&abi, ptr) } >= 1);
        assert_eq!(unsafe { stub_live_sessions(&abi) }, live);
        assert_eq!(unsafe { stub_free_calls(&abi) }, frees);
        drop(session);
        assert_eq!(unsafe { stub_live_sessions(&abi) }, live - 1);
        assert_eq!(unsafe { stub_free_calls(&abi) }, frees + 1);
    }

    fn alloc_option_none_member(heap: &mut Heap) -> Member {
        let none = crate::io::alloc_option_none(heap);
        match heap.find_object_by_addr(none.raw() as u64) {
            Some(obj) => Member::Object(obj),
            None => Member::Value(none),
        }
    }

    fn alloc_string_member(heap: &mut Heap, s: &str) -> Member {
        let gc = heap.intern(s.to_string());
        Member::Object(Object::String(gc))
    }

    fn client_enable_opts(heap: &mut Heap) -> Value {
        use crate::memory::ObjInstance;
        let (obj, mut gc) = heap.alloc(ObjInstance::default(), Object::Instance);
        let inst = gc.as_mut();
        let intern = |h: &mut Heap, n: &str| h.intern(n.to_string());
        let k_verify = intern(heap, "verify");
        inst.set(k_verify, Member::Value(Value::from(false)));
        let k_ca_pem = intern(heap, "ca_pem");
        inst.set(k_ca_pem, alloc_option_none_member(heap));
        let k_ca_path = intern(heap, "ca_path");
        inst.set(k_ca_path, alloc_option_none_member(heap));
        let k_timeout = intern(heap, "timeout_ms");
        inst.set(k_timeout, Member::Value(Value::from(25i64)));
        let k_alpn = intern(heap, "alpn");
        inst.set(k_alpn, alloc_string_member(heap, ""));
        Value::from(obj.addr())
    }

    fn server_enable_opts(heap: &mut Heap) -> Value {
        use crate::memory::ObjInstance;
        let (obj, mut gc) = heap.alloc(ObjInstance::default(), Object::Instance);
        let inst = gc.as_mut();
        let intern = |h: &mut Heap, n: &str| h.intern(n.to_string());
        inst.set(intern(heap, "cert_pem"), alloc_string_member(heap, "cert"));
        inst.set(intern(heap, "key_pem"), alloc_string_member(heap, "key"));
        inst.set(
            intern(heap, "timeout_ms"),
            Member::Value(Value::from(25i64)),
        );
        inst.set(intern(heap, "client_ca_pem"), alloc_string_member(heap, ""));
        inst.set(intern(heap, "alpn"), alloc_string_member(heap, ""));
        Value::from(obj.addr())
    }

    #[test]
    fn leftover_enable_without_abi_leaves_tcp() {
        let _guard = stub_lock();
        reset_preferred();
        let (client, server) = tcp_pair();
        let mut heap = Heap::default();
        let stream =
            alloc_stream(&mut heap, NativeHandle::Tcp(client), StreamKind::Tcp).expect("alloc");
        let opts = client_enable_opts(&mut heap);
        let err = crate::tls::tls_client_enable(&mut heap, stream, "localhost", opts).unwrap_err();
        assert_eq!(err, IoErrorTag::Other);
        assert_eq!(
            with_stream_mut(&mut heap, stream, |s| s.kind).unwrap(),
            StreamKind::Tcp
        );
        assert!(with_stream_mut(&mut heap, stream, |s| s.tls.is_none()).unwrap());
        drop(server);
    }

    #[test]
    fn leftover_hostinvoke_enable_attaches_and_parks_would_block() {
        let Some(abi) = load_stub() else {
            eprintln!("skip: cc could not build tls ABI stub");
            return;
        };
        let _guard = stub_lock();
        reset_preferred();
        install_preferred(abi.clone());
        unsafe { stub_set_next_enable_err(&abi, ABI_OK) };
        let (client, server) = tcp_pair();
        let mut heap = Heap::default();
        let stream =
            alloc_stream(&mut heap, NativeHandle::Tcp(client), StreamKind::Tcp).expect("alloc");
        let enable0 = unsafe { stub_enable_calls(&abi) };
        let free0 = unsafe { stub_free_calls(&abi) };
        unsafe { stub_set_next_enable_err(&abi, IoErrorTag::WouldBlock as i32) };

        let opts = client_enable_opts(&mut heap);
        let out = crate::tls::tls_client_enable(&mut heap, stream, "localhost", opts)
            .expect("leftover enable returns the same Stream");
        assert_eq!(out, stream);
        assert_eq!(
            with_stream_mut(&mut heap, stream, |s| s.kind).unwrap(),
            StreamKind::Tls
        );
        assert!(
            with_stream_mut(&mut heap, stream, |s| {
                s.tls.as_ref().is_some_and(|t| t.wants_write())
            })
            .unwrap()
        );
        assert_eq!(unsafe { stub_enable_calls(&abi) }, enable0 + 1);
        assert_eq!(unsafe { stub_free_calls(&abi) }, free0);

        let buf = make_byte_array(&mut heap, &[0; 16]);
        let n = stream_read(&mut heap, stream, buf).expect("read continues handshake");
        assert_eq!(n, Some(5));
        assert_eq!(unsafe { stub_enable_calls(&abi) }, enable0 + 1);

        crate::tls::tls_client_disable(&mut heap, stream).expect("disable");
        assert_eq!(
            with_stream_mut(&mut heap, stream, |s| s.kind).unwrap(),
            StreamKind::Tcp
        );
        reset_preferred();
        drop(server);
    }

    #[test]
    fn leftover_server_enable_and_alpn() {
        let Some(abi) = load_stub() else {
            eprintln!("skip: cc could not build tls ABI stub");
            return;
        };
        let _guard = stub_lock();
        reset_preferred();
        install_preferred(abi.clone());
        unsafe { stub_set_next_enable_err(&abi, ABI_OK) };
        let (client, server) = tcp_pair();
        let mut heap = Heap::default();
        let stream =
            alloc_stream(&mut heap, NativeHandle::Tcp(server), StreamKind::Tcp).expect("alloc");
        let err = crate::tls::tls_alpn_protocol(&mut heap, stream).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        let opts = server_enable_opts(&mut heap);
        crate::tls::tls_server_enable(&mut heap, stream, opts).expect("server enable");
        let proto = crate::tls::tls_alpn_protocol(&mut heap, stream).expect("alpn");
        let s = crate::io::value_as_string(&heap, proto).expect("alpn string");
        assert_eq!(s, "h2");
        crate::tls::tls_server_disable(&mut heap, stream).ok();
        reset_preferred();
        drop(client);
    }

    #[test]
    fn leftover_enable_non_tcp_is_invalid_input() {
        let Some(abi) = load_stub() else {
            eprintln!("skip: cc could not build tls ABI stub");
            return;
        };
        let _guard = stub_lock();
        reset_preferred();
        install_preferred(abi);
        let mut heap = Heap::default();
        let path = std::env::temp_dir().join("coil_tls_leftover_file.bin");
        let _ = std::fs::write(&path, b"x");
        let stream = crate::io::stream_open(&mut heap, path.to_str().unwrap(), "r").expect("open");
        let opts = client_enable_opts(&mut heap);
        let err = crate::tls::tls_client_enable(&mut heap, stream, "localhost", opts).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        reset_preferred();
        let _ = std::fs::remove_file(path);
    }

    fn real_libtls_path() -> Option<PathBuf> {
        if let Ok(p) = std::env::var("COIL_TLS_NATIVE") {
            let pb = PathBuf::from(&p);
            if pb.is_file() {
                return Some(pb);
            }
            let so = pb.join(platform_shared_lib_filename("tls"));
            if so.is_file() {
                return Some(so);
            }
        }
        let root = workspace_root();
        let candidates = [
            PathBuf::from("/tmp/coil-tls/native/libtls.so"),
            PathBuf::from("/tmp/coil-tls/native/target/release/libtls.so"),
            root.join("../coil-tls/native/libtls.so"),
            root.join("../coil-tls/native/target/release/libtls.so"),
        ];
        candidates.into_iter().find(|p| p.is_file())
    }

    fn load_real_libtls() -> Option<Arc<TlsNativeAbi>> {
        TlsNativeAbi::load_path(&real_libtls_path()?).ok()
    }

    fn test_server_pem() -> (String, String) {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let cert = std::fs::read_to_string(dir.join("cert.pem")).expect("cert.pem");
        let key = std::fs::read_to_string(dir.join("key.pem")).expect("key.pem");
        (cert, key)
    }

    fn client_enable_opts_full(heap: &mut Heap, timeout_ms: i64, alpn: &str) -> Value {
        use crate::memory::ObjInstance;
        let (obj, mut gc) = heap.alloc(ObjInstance::default(), Object::Instance);
        let inst = gc.as_mut();
        let intern = |h: &mut Heap, n: &str| h.intern(n.to_string());
        inst.set(intern(heap, "verify"), Member::Value(Value::from(false)));
        inst.set(intern(heap, "ca_pem"), alloc_option_none_member(heap));
        inst.set(intern(heap, "ca_path"), alloc_option_none_member(heap));
        inst.set(
            intern(heap, "timeout_ms"),
            Member::Value(Value::from(timeout_ms)),
        );
        inst.set(intern(heap, "alpn"), alloc_string_member(heap, alpn));
        Value::from(obj.addr())
    }

    fn server_enable_opts_full(
        heap: &mut Heap,
        cert_pem: &str,
        key_pem: &str,
        timeout_ms: i64,
        alpn: &str,
    ) -> Value {
        use crate::memory::ObjInstance;
        let (obj, mut gc) = heap.alloc(ObjInstance::default(), Object::Instance);
        let inst = gc.as_mut();
        let intern = |h: &mut Heap, n: &str| h.intern(n.to_string());
        inst.set(
            intern(heap, "cert_pem"),
            alloc_string_member(heap, cert_pem),
        );
        inst.set(intern(heap, "key_pem"), alloc_string_member(heap, key_pem));
        inst.set(
            intern(heap, "timeout_ms"),
            Member::Value(Value::from(timeout_ms)),
        );
        inst.set(intern(heap, "client_ca_pem"), alloc_string_member(heap, ""));
        inst.set(intern(heap, "alpn"), alloc_string_member(heap, alpn));
        Value::from(obj.addr())
    }

    /// coil-tls `coil_tls_disable` also frees; leftover Drop would double-free.
    fn leak_tls_slot(heap: &mut Heap, stream: Value) {
        let _ = with_stream_mut(heap, stream, |s| {
            if let Some(slot) = s.tls.take() {
                std::mem::forget(slot);
            }
            s.kind = StreamKind::Tcp;
        });
    }

    #[test]
    fn leftover_abi_real_libtls_enable_would_block_keeps_session() {
        let Some(abi) = load_real_libtls() else {
            eprintln!("skip: real libtls.so not found (set COIL_TLS_NATIVE)");
            return;
        };
        let _guard = stub_lock();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hold = std::thread::spawn(move || {
            let Ok((sock, _)) = listener.accept() else {
                return;
            };
            std::thread::sleep(Duration::from_millis(300));
            drop(sock);
        });
        let client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client.set_nonblocking(true).ok();
        let handle = NativeHandle::Tcp(client);
        let fd = handle.tls_abi_fd();
        let outcome = abi.client_enable(fd, "127.0.0.1", false, None, None, 0, "");
        assert!(
            matches!(outcome, NativeEnable::WouldBlock(_)),
            "leftover must decode coil-tls string err_out as WouldBlock"
        );
        drop(outcome);
        drop(handle);
        let _ = hold.join();
    }

    /// COI-116: leftover client+server enable on a TCP pair with HostState bound.
    #[test]
    fn leftover_server_client_enable_with_host_state_bound() {
        let Some(abi) = load_real_libtls() else {
            eprintln!("skip: real libtls.so not found (set COIL_TLS_NATIVE)");
            return;
        };
        let _guard = stub_lock();
        reset_preferred();
        install_preferred(abi);
        let (cert_pem, key_pem) = test_server_pem();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let reactor = crate::reactor::Reactor::new(1);
        let server_reactor = std::sync::Arc::clone(&reactor);
        let server = std::thread::spawn(move || {
            let mut vm = crate::Machine::<64>::default();
            vm.set_reactor(std::sync::Arc::clone(&server_reactor));
            let _guard = crate::thread::HostStateGuard::enter(&mut vm);
            let mut heap = Heap::default();
            ready_tx.send(()).ok();
            let Ok((sock, _)) = listener.accept() else {
                panic!("accept");
            };
            let s =
                alloc_stream(&mut heap, NativeHandle::Tcp(sock), StreamKind::Tcp).expect("stream");
            let opts = server_enable_opts_full(&mut heap, &cert_pem, &key_pem, 5000, "h2");
            let s = crate::tls::tls_server_enable(&mut heap, s, opts).expect("server enable");
            let proto = crate::tls::tls_alpn_protocol(&mut heap, s).expect("alpn");
            let name = crate::io::value_as_string(&heap, proto).expect("alpn string");
            leak_tls_slot(&mut heap, s);
            name
        });
        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("ready");
        let mut vm = crate::Machine::<64>::default();
        vm.set_reactor(std::sync::Arc::clone(&reactor));
        let _guard = crate::thread::HostStateGuard::enter(&mut vm);
        let mut heap = Heap::default();
        let s = crate::io::tcp_connect(&mut heap, "localhost", port as i64).expect("tcp");
        let opts = client_enable_opts_full(&mut heap, 5000, "h2");
        let s =
            crate::tls::tls_client_enable(&mut heap, s, "localhost", opts).expect("client enable");
        let proto = crate::tls::tls_alpn_protocol(&mut heap, s).expect("alpn");
        let client_alpn = crate::io::value_as_string(&heap, proto).expect("alpn string");
        leak_tls_slot(&mut heap, s);
        let server_alpn = server.join().expect("server");
        reactor.shutdown();
        reset_preferred();
        assert_eq!(client_alpn, "h2");
        assert_eq!(server_alpn, "h2");
    }

    /// COI-116: leftover handshake parks must not `help_once` CPU jobs.
    #[test]
    fn leftover_handshake_wait_does_not_help_steal_cpu_jobs() {
        use crate::ffi::Natives;
        use crate::reactor::{Job, Reactor};
        use crate::thread::{HostStateGuard, JoinState, ThreadProgram};
        use common::{Byte, Instruction, ProgramDebug};

        let Some(abi) = load_real_libtls() else {
            eprintln!("skip: real libtls.so not found (set COIL_TLS_NATIVE)");
            return;
        };
        let _guard = stub_lock();
        reset_preferred();
        install_preferred(abi);

        fn const_job(reactor: &std::sync::Arc<Reactor>, imm: i32) -> std::sync::Arc<JoinState> {
            let state = std::sync::Arc::new(JoinState::new());
            let code = vec![
                Byte::new(Instruction::CONST).with_value_u32(imm as u32),
                Byte::new(Instruction::RETURN),
            ];
            let program = std::sync::Arc::new(ThreadProgram {
                code: std::sync::Arc::new(code),
                constants: std::sync::Arc::new(Vec::new()),
                strings: std::sync::Arc::new(Vec::new()),
                static_slot_count: 0,
                debug: ProgramDebug::default(),
                operand_stack_slots: crate::DEFAULT_OPERAND_STACK_SLOTS as u32,
            });
            reactor.submit(Job {
                entry: 0,
                args: Vec::new(),
                state: std::sync::Arc::clone(&state),
                program,
                natives: Natives::new(),
                shared_print: None,
                live_threads: crate::thread::new_live_thread_registry(),
                reactor: std::sync::Arc::clone(reactor),
                io_reactor: crate::io_reactor::IoReactor::new(),
            });
            state
        }

        let reactor = Reactor::new(1);
        let warmup = const_job(&reactor, 0);
        let _ = reactor.wait_join(&warmup);
        reactor.shutdown();

        let pending = const_job(&reactor, 99);
        assert!(
            pending.try_take_result().is_none(),
            "job must sit in injector after shutdown"
        );

        let mut vm = crate::Machine::<64>::default();
        vm.set_reactor(std::sync::Arc::clone(&reactor));
        let _hs = HostStateGuard::enter(&mut vm);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let peer = std::thread::spawn(move || {
            let _ = listener.accept();
            std::thread::sleep(Duration::from_millis(150));
        });
        let mut heap = Heap::default();
        let s = crate::io::tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let opts = client_enable_opts_full(&mut heap, 100, "");
        let _ = crate::tls::tls_client_enable(&mut heap, s, "127.0.0.1", opts);
        leak_tls_slot(&mut heap, s);
        let _ = peer.join();

        assert!(
            pending.try_take_result().is_none(),
            "TLS handshake wait must not help-steal CPU jobs (COI-116)"
        );
        reactor.help_once();
        assert_eq!(
            pending.try_take_result().expect("helped job"),
            Ok(crate::thread::PortableValue::Immediate(99))
        );
        reset_preferred();
    }
}
