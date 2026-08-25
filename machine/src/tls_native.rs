//! Dloaded `coil_tls_*` ABI used by [`StreamKind::Tls`](crate::memory::StreamKind).
//!
//! In-tree rustls ([`crate::tls`]) stays the enable/read fallback while the
//! `tls` Cargo feature is on. When a stream holds a [`NativeTlsSession`],
//! `stream_read` / `stream_write` / close / disable / ALPN call these symbols
//! instead. Handshake parks stay in the VM ([`crate::io::reactor_wait_fd_no_help`]).

use std::ffi::{CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::sync::{Arc, Mutex, OnceLock};

use libloading::Library;

use crate::ffi::{FfiError, resolve_library};
use crate::io::IoErrorTag;
use crate::io_handle::NativeHandle;
use crate::memory::{Heap, ObjStream, StreamKind};

/// `err_out` value meaning success (not an [`IoErrorTag`] discriminant).
pub const ABI_OK: i32 = -1;

type ClientEnableFn = unsafe extern "C" fn(
    i64,
    *const c_char,
    i32,
    *const c_char,
    *const c_char,
    i64,
    *const c_char,
    *mut i32,
) -> *mut c_void;
type ServerEnableFn = unsafe extern "C" fn(
    i64,
    *const c_char,
    *const c_char,
    i64,
    *const c_char,
    *const c_char,
    *mut i32,
) -> *mut c_void;
type ReadFn = unsafe extern "C" fn(*mut c_void, i64, *mut u8, usize, *mut i32) -> isize;
type WriteFn = unsafe extern "C" fn(*mut c_void, i64, *const u8, usize, *mut i32) -> isize;
type AlpnFn = unsafe extern "C" fn(*mut c_void, *mut u8, usize) -> isize;
type DisableFn = unsafe extern "C" fn(*mut c_void, i64, *mut i32) -> i32;
type FreeFn = unsafe extern "C" fn(*mut c_void);

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
    pub fn client_enable(
        self: &Arc<Self>,
        fd: i64,
        host: &str,
        verify: bool,
        ca_pem: Option<&str>,
        ca_path: Option<&str>,
        timeout_ms: i64,
        alpn: &str,
    ) -> Result<NativeTlsSession, IoErrorTag> {
        let host = c_string(host)?;
        let ca_pem = optional_c_string(ca_pem)?;
        let ca_path = optional_c_string(ca_path)?;
        let alpn = c_string(alpn)?;
        let mut err = ABI_OK;
        let ptr = unsafe {
            (self.client_enable)(
                fd,
                host.as_ptr(),
                i32::from(verify),
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
    ) -> Result<NativeTlsSession, IoErrorTag> {
        let cert = c_string(cert_pem)?;
        let key = c_string(key_pem)?;
        let client_ca = c_string(client_ca_pem)?;
        let alpn = c_string(alpn)?;
        let mut err = ABI_OK;
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

fn session_from_enable(
    abi: Arc<TlsNativeAbi>,
    ptr: *mut c_void,
    err: i32,
) -> Result<NativeTlsSession, IoErrorTag> {
    if err != ABI_OK {
        if !ptr.is_null() {
            unsafe { (abi.free)(ptr) };
        }
        return Err(IoErrorTag::from_abi(err));
    }
    let ptr = NonNull::new(ptr).ok_or(IoErrorTag::Other)?;
    Ok(NativeTlsSession {
        ptr: Some(ptr),
        abi,
    })
}

/// Opaque rustls session living in dloaded `libtls`.
pub struct NativeTlsSession {
    ptr: Option<NonNull<c_void>>,
    abi: Arc<TlsNativeAbi>,
}

unsafe impl Send for NativeTlsSession {}
unsafe impl Sync for NativeTlsSession {}

impl NativeTlsSession {
    fn raw(&self) -> *mut c_void {
        self.ptr.map(NonNull::as_ptr).unwrap_or(ptr::null_mut())
    }

    fn take_ptr(&mut self) -> *mut c_void {
        self.ptr
            .take()
            .map(NonNull::as_ptr)
            .unwrap_or(ptr::null_mut())
    }

    pub fn read(&self, fd: i64, buf: &mut [u8]) -> Result<Option<usize>, IoErrorTag> {
        let ptr = self.raw();
        if ptr.is_null() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        let mut err = ABI_OK;
        let n = unsafe { (self.abi.read)(ptr, fd, buf.as_mut_ptr(), buf.len(), &mut err) };
        if err != ABI_OK {
            return Err(IoErrorTag::from_abi(err));
        }
        if n < 0 {
            return Err(IoErrorTag::Other);
        }
        if n == 0 {
            Ok(None)
        } else {
            Ok(Some(n as usize))
        }
    }

    pub fn write(&self, fd: i64, buf: &[u8]) -> Result<usize, IoErrorTag> {
        let ptr = self.raw();
        if ptr.is_null() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        let mut err = ABI_OK;
        let n = unsafe { (self.abi.write)(ptr, fd, buf.as_ptr(), buf.len(), &mut err) };
        if err != ABI_OK {
            return Err(IoErrorTag::from_abi(err));
        }
        if n < 0 {
            return Err(IoErrorTag::Other);
        }
        Ok(n as usize)
    }

    pub fn alpn_protocol(&self) -> String {
        let ptr = self.raw();
        if ptr.is_null() {
            return String::new();
        }
        let mut buf = [0u8; 256];
        let n = unsafe { (self.abi.alpn)(ptr, buf.as_mut_ptr(), buf.len()) };
        if n <= 0 {
            return String::new();
        }
        String::from_utf8_lossy(&buf[..n as usize]).into_owned()
    }

    /// `close_notify` + free the session in the `.so`.
    pub fn disable(mut self, fd: i64) -> Result<(), IoErrorTag> {
        let ptr = self.take_ptr();
        if ptr.is_null() {
            return Ok(());
        }
        let mut err = ABI_OK;
        unsafe { (self.abi.disable)(ptr, fd, &mut err) };
        if err != ABI_OK {
            Err(IoErrorTag::from_abi(err))
        } else {
            Ok(())
        }
    }
}

impl Drop for NativeTlsSession {
    fn drop(&mut self) {
        let ptr = self.take_ptr();
        if !ptr.is_null() {
            unsafe { (self.abi.free)(ptr) };
        }
    }
}

/// TLS session stored on [`ObjStream`]: in-tree rustls or a native `.so` pointer.
pub enum TlsSessionSlot {
    /// In-tree rustls (feature `tls` fallback when the stream was enabled here).
    #[cfg(feature = "tls")]
    InTree(Box<crate::tls::TlsSession>),
    /// Session owned by dloaded `libtls`.
    Native(NativeTlsSession),
}

impl TlsSessionSlot {
    pub fn has_buffered_plaintext(&self) -> bool {
        match self {
            #[cfg(feature = "tls")]
            Self::InTree(t) => t.has_buffered_plaintext(),
            Self::Native(_) => false,
        }
    }

    pub fn wants_write(&self) -> bool {
        match self {
            #[cfg(feature = "tls")]
            Self::InTree(t) => t.wants_write(),
            Self::Native(_) => false,
        }
    }

    pub fn alpn_protocol(&self) -> String {
        match self {
            #[cfg(feature = "tls")]
            Self::InTree(t) => t.alpn_protocol(),
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
        #[cfg(feature = "tls")]
        TlsSessionSlot::InTree(tls) => crate::tls::tls_read(handle, tls, buf),
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
        #[cfg(feature = "tls")]
        TlsSessionSlot::InTree(tls) => crate::tls::tls_write(handle, tls, buf),
        TlsSessionSlot::Native(n) => n.write(handle.tls_abi_fd(), buf),
    }
}

/// Best-effort `close_notify` / `coil_tls_disable` before dropping the slot.
pub fn drop_slot(handle: Option<&mut NativeHandle>, slot: TlsSessionSlot) {
    match slot {
        #[cfg(feature = "tls")]
        TlsSessionSlot::InTree(mut tls) => {
            if let Some(h) = handle {
                let _ = crate::tls::send_close_notify(h, &mut tls);
            }
        }
        TlsSessionSlot::Native(n) => {
            if let Some(h) = handle {
                let _ = n.disable(h.tls_abi_fd());
            }
        }
    }
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

/// Remember a `dload("tls")` so later HostInvoke enable can prefer the `.so`.
pub fn note_loaded_library(name: &str, lib: Arc<Library>) {
    if !crate::ffi::library_stem(name).eq_ignore_ascii_case("tls") {
        return;
    }
    if let Ok(abi) = TlsNativeAbi::from_library(lib) {
        set_preferred(abi);
    }
}

/// Try to resolve `libtls` on VM FFI search paths (no-op if missing).
pub fn note_search_paths(base_dir: Option<&Path>, search_paths: &[PathBuf]) {
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

fn set_preferred(abi: Arc<TlsNativeAbi>) {
    *preferred_slot().lock().unwrap_or_else(|e| e.into_inner()) = Some(abi);
}

fn preferred_slot() -> &'static Mutex<Option<Arc<TlsNativeAbi>>> {
    static SLOT: OnceLock<Mutex<Option<Arc<TlsNativeAbi>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
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
    use crate::memory::{Heap, ObjArray, Object};
    use common::Value;
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::sync::OnceLock;
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

    unsafe fn stub_set_would_block_reads(abi: &TlsNativeAbi, session: *mut c_void, n: i32) {
        type Fn = unsafe extern "C" fn(*mut c_void, i32);
        let f: libloading::Symbol<Fn> = unsafe {
            abi.library()
                .get(b"coil_tls_stub_set_would_block_reads\0")
                .expect("stub hook")
        };
        unsafe { f(session, n) };
    }

    unsafe fn stub_read_calls(abi: &TlsNativeAbi, session: *mut c_void) -> i32 {
        type Fn = unsafe extern "C" fn(*mut c_void) -> i32;
        let f: libloading::Symbol<Fn> = unsafe {
            abi.library()
                .get(b"coil_tls_stub_read_calls\0")
                .expect("stub hook")
        };
        unsafe { f(session) }
    }

    unsafe fn stub_write_calls(abi: &TlsNativeAbi, session: *mut c_void) -> i32 {
        type Fn = unsafe extern "C" fn(*mut c_void) -> i32;
        let f: libloading::Symbol<Fn> = unsafe {
            abi.library()
                .get(b"coil_tls_stub_write_calls\0")
                .expect("stub hook")
        };
        unsafe { f(session) }
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
        let session = abi
            .client_enable(fd, "localhost", false, None, None, 0, "")
            .expect("stub enable");
        attach_native(heap, stream, session).expect("attach");
        stream
    }

    fn native_ptr(heap: &mut Heap, stream: Value) -> *mut c_void {
        with_stream_mut(heap, stream, |s| match s.tls.as_ref() {
            Some(TlsSessionSlot::Native(n)) => n.raw(),
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
        let (client, server) = tcp_pair();
        let mut heap = Heap::default();
        let stream =
            alloc_stream(&mut heap, NativeHandle::Tcp(client), StreamKind::Tcp).expect("alloc");
        // Failed attach: kind stays Tcp, no session (drop the session without attach).
        let fd = with_stream_mut(&mut heap, stream, |s| {
            s.handle.as_ref().map(|h| h.tls_abi_fd()).unwrap_or(-1)
        })
        .unwrap();
        let session = abi
            .client_enable(fd, "localhost", false, None, None, 0, "")
            .expect("enable");
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
        type SetWrites = unsafe extern "C" fn(*mut c_void, i32);
        let set_w: libloading::Symbol<SetWrites> = unsafe {
            abi.library()
                .get(b"coil_tls_stub_set_would_block_writes\0")
                .expect("hook")
        };
        unsafe { set_w(ptr, 1) };
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
        let dir = path.parent().unwrap();
        let abi = TlsNativeAbi::resolve(None, &[dir.to_path_buf()]).expect("resolve tls");
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
        let session = abi
            .client_enable(fd, "localhost", false, None, None, 0, "h2")
            .expect("enable");
        drop(session);
        drop(client);
        drop(server);
    }
}
