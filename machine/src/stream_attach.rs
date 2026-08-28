//! Generic in-place Stream attach: package IO via a C vtable.
//!
//! The VM stores fd, attached kind, session pointer, and hooks. A new package
//! kind does not require a VM change. Handshake is one native step per call;
//! WouldBlock parks on [`crate::io::reactor_wait_fd_no_help`].

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_void};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};

use common::Value;

use crate::io::{IoErrorTag, reactor_wait_fd_no_help, stream_wait_handle, with_stream_mut};
use crate::io_handle::NativeHandle;
use crate::io_reactor::Interest;
use crate::memory::{Heap, ObjStream, StreamKind};

/// C hooks registered by [`stream_attach`]. `shutdown` must not free.
pub type StreamReadFn =
    unsafe extern "C" fn(*mut c_void, *mut u8, usize, *mut *const c_char) -> i64;
pub type StreamWriteFn =
    unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut *const c_char) -> i64;
pub type StreamShutdownFn = unsafe extern "C" fn(*mut c_void, *mut *const c_char) -> i32;
pub type StreamFreeFn = unsafe extern "C" fn(*mut c_void);

/// Package session pointer plus C vtable living on the Stream object.
pub struct StreamVTable {
    pub read: StreamReadFn,
    pub write: StreamWriteFn,
    pub shutdown: StreamShutdownFn,
    pub free: StreamFreeFn,
}

/// Attached native IO. Session memory lives in the package `.so`.
pub struct AttachedIo {
    ptr: Option<NonNull<c_void>>,
    vtable: StreamVTable,
    wants_write: Cell<bool>,
}

unsafe impl Send for AttachedIo {}
unsafe impl Sync for AttachedIo {}

impl AttachedIo {
    /// Generic attach from compiler-known `Stream.attach`.
    pub fn from_vtable(ptr: NonNull<c_void>, vtable: StreamVTable) -> Self {
        Self {
            ptr: Some(ptr),
            vtable,
            wants_write: Cell::new(false),
        }
    }

    pub fn wants_write(&self) -> bool {
        self.wants_write.get()
    }

    pub fn set_wants_write(&self, wants: bool) {
        self.wants_write.set(wants);
    }

    fn raw_ptr(&self) -> *mut c_void {
        self.ptr
            .map(NonNull::as_ptr)
            .unwrap_or(std::ptr::null_mut())
    }

    /// Non-blocking app read through the vtable.
    pub fn read(&self, buf: &mut [u8]) -> Result<Option<usize>, IoErrorTag> {
        let ptr = self.raw_ptr();
        if ptr.is_null() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        let mut err: *const c_char = std::ptr::null();
        let n = unsafe { (self.vtable.read)(ptr, buf.as_mut_ptr(), buf.len(), &mut err) };
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
            if buf.is_empty() {
                Ok(Some(0))
            } else {
                Ok(None)
            }
        } else {
            Ok(Some(n as usize))
        }
    }

    /// Non-blocking app write through the vtable.
    pub fn write(&self, buf: &[u8]) -> Result<usize, IoErrorTag> {
        let ptr = self.raw_ptr();
        if ptr.is_null() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        let mut err: *const c_char = std::ptr::null();
        let n = unsafe { (self.vtable.write)(ptr, buf.as_ptr(), buf.len(), &mut err) };
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

    /// One empty write then empty read. Handshake / probe; not a VM loop.
    pub fn handshake_step(&self) -> Result<(), IoErrorTag> {
        match self.write(&[]) {
            Ok(_) => Ok(()),
            Err(IoErrorTag::WouldBlock) => match self.read(&mut []) {
                Ok(_) => Ok(()),
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        }
    }

    /// close_notify-style shutdown. Must not free.
    pub fn shutdown(&self) -> Result<(), IoErrorTag> {
        let ptr = self.raw_ptr();
        if ptr.is_null() {
            return Ok(());
        }
        let mut err: *const c_char = std::ptr::null();
        let _rc = unsafe { (self.vtable.shutdown)(ptr, &mut err) };
        if let Some(tag) = tag_from_err_out(err) {
            Err(tag)
        } else {
            Ok(())
        }
    }

    /// Drop the session pointer via `free`. Clears `ptr`.
    pub fn free(&mut self) {
        if let Some(p) = self.ptr.take() {
            unsafe { (self.vtable.free)(p.as_ptr()) };
        }
    }

    /// Shutdown when `handle` still has a usable fd, then free.
    pub fn shutdown_then_free(mut self, handle: Option<&mut NativeHandle>) {
        if handle.is_some() {
            let _ = self.shutdown();
        }
        self.free();
    }

    #[cfg(test)]
    pub(crate) fn session_ptr(&self) -> *mut c_void {
        self.raw_ptr()
    }
}

impl Drop for AttachedIo {
    fn drop(&mut self) {
        self.free();
    }
}

fn tag_from_err_out(err: *const c_char) -> Option<IoErrorTag> {
    if err.is_null() {
        return None;
    }
    let name = unsafe { CStr::from_ptr(err) }.to_str().unwrap_or("");
    if name.is_empty() {
        None
    } else {
        Some(IoErrorTag::from_abi_name(name))
    }
}

/// When false, [`stream_attach`] returns `PermissionDenied`. Set from
/// `coil.toml` `[ffi] allow_attach` (default off). Unsigned C vtables are
/// not a default coil capability.
static ALLOW_ATTACH: AtomicBool = AtomicBool::new(false);

/// Runtime gate for `Stream.attach` (from project manifest).
pub fn set_allow_attach(allow: bool) {
    ALLOW_ATTACH.store(allow, Ordering::Relaxed);
}

fn fn_from_i64<T>(addr: i64) -> Result<T, IoErrorTag> {
    if addr == 0 {
        return Err(IoErrorTag::InvalidInput);
    }
    Ok(unsafe { std::mem::transmute_copy(&addr) })
}

/// In-place attach on the same Stream object.
pub fn stream_attach(
    heap: &mut Heap,
    stream: Value,
    ptr: i64,
    read: i64,
    write: i64,
    shutdown: i64,
    free: i64,
) -> Result<Value, IoErrorTag> {
    if !ALLOW_ATTACH.load(Ordering::Relaxed) {
        return Err(IoErrorTag::PermissionDenied);
    }
    let ptr_nn = NonNull::new(ptr as *mut c_void).ok_or(IoErrorTag::InvalidInput)?;
    let vtable = StreamVTable {
        read: fn_from_i64(read)?,
        write: fn_from_i64(write)?,
        shutdown: fn_from_i64(shutdown)?,
        free: fn_from_i64(free)?,
    };
    with_stream_mut(heap, stream, |s: &mut ObjStream| {
        if s.closed || s.handle.is_none() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        if s.kind == StreamKind::Attached || s.attached.is_some() {
            return Err(IoErrorTag::InvalidInput);
        }
        s.kind = StreamKind::Attached;
        s.attached = Some(AttachedIo::from_vtable(ptr_nn, vtable));
        Ok(())
    })??;
    Ok(stream)
}

/// Park this coro on the stream fd without help-steal (COI-116 / COI-165).
pub fn stream_park(heap: &mut Heap, stream: Value) -> Result<(), IoErrorTag> {
    let wait = stream_wait_handle(heap, stream)?;
    let wants_write = with_stream_mut(heap, stream, |s| {
        s.attached.as_ref().is_some_and(|a| a.wants_write())
    })?;
    let timeout = with_stream_mut(heap, stream, |s| {
        if wants_write {
            s.write_timeout
        } else {
            s.read_timeout
        }
    })?;
    let interest = if wants_write {
        Interest::Writable
    } else {
        Interest::Readable
    };
    reactor_wait_fd_no_help(wait, interest, timeout)
}

/// True when this stream dispatches read/write/close through an attached vtable.
#[allow(dead_code)]
pub fn stream_is_attached(s: &ObjStream) -> bool {
    s.kind == StreamKind::Attached && s.attached.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::{
        alloc_stream, stream_close, stream_read, stream_set_read_timeout, stream_write,
    };
    use crate::io_handle::NativeHandle;
    use crate::memory::{ObjArray, Object};
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicI32, AtomicU8, Ordering};
    use std::sync::{Mutex, MutexGuard};

    static ATTACH_TEST_GUARD: Mutex<()> = Mutex::new(());

    struct AttachAllowGuard {
        prev: bool,
        _lock: MutexGuard<'static, ()>,
    }

    impl AttachAllowGuard {
        fn allow() -> Self {
            let lock = ATTACH_TEST_GUARD.lock().expect("attach test mutex");
            let prev = ALLOW_ATTACH.load(Ordering::Relaxed);
            ALLOW_ATTACH.store(true, Ordering::Relaxed);
            Self { prev, _lock: lock }
        }

        fn deny() -> Self {
            let lock = ATTACH_TEST_GUARD.lock().expect("attach test mutex");
            let prev = ALLOW_ATTACH.load(Ordering::Relaxed);
            ALLOW_ATTACH.store(false, Ordering::Relaxed);
            Self { prev, _lock: lock }
        }
    }

    impl Drop for AttachAllowGuard {
        fn drop(&mut self) {
            ALLOW_ATTACH.store(self.prev, Ordering::Relaxed);
        }
    }
    use std::time::Duration;

    struct XorSession {
        xor: u8,
        shutdowns: AtomicI32,
        frees: AtomicI32,
        would_block_reads: AtomicI32,
        would_block_writes: AtomicI32,
        buf: [u8; 64],
        len: AtomicU8,
    }

    impl XorSession {
        fn new(xor: u8) -> Box<Self> {
            Box::new(Self {
                xor,
                shutdowns: AtomicI32::new(0),
                frees: AtomicI32::new(0),
                would_block_reads: AtomicI32::new(0),
                would_block_writes: AtomicI32::new(0),
                buf: [0; 64],
                len: AtomicU8::new(0),
            })
        }
    }

    unsafe extern "C" fn xor_read(
        ptr: *mut c_void,
        buf: *mut u8,
        len: usize,
        err: *mut *const c_char,
    ) -> i64 {
        let s = unsafe { &*(ptr as *const XorSession) };
        if s.would_block_reads.load(Ordering::SeqCst) > 0 {
            s.would_block_reads.fetch_sub(1, Ordering::SeqCst);
            if !err.is_null() {
                unsafe { *err = c"WouldBlock".as_ptr() };
            }
            return 0;
        }
        let n = (s.len.load(Ordering::SeqCst) as usize).min(len);
        for i in 0..n {
            unsafe { *buf.add(i) = s.buf[i] ^ s.xor };
        }
        s.len.store(0, Ordering::SeqCst);
        n as i64
    }

    unsafe extern "C" fn xor_write(
        ptr: *mut c_void,
        buf: *const u8,
        len: usize,
        err: *mut *const c_char,
    ) -> i64 {
        let s = unsafe { &mut *(ptr as *mut XorSession) };
        if s.would_block_writes.load(Ordering::SeqCst) > 0 {
            s.would_block_writes.fetch_sub(1, Ordering::SeqCst);
            if !err.is_null() {
                unsafe { *err = c"WouldBlock".as_ptr() };
            }
            return 0;
        }
        let n = len.min(s.buf.len());
        for i in 0..n {
            s.buf[i] = unsafe { *buf.add(i) } ^ s.xor;
        }
        s.len.store(n as u8, Ordering::SeqCst);
        n as i64
    }

    unsafe extern "C" fn xor_shutdown(ptr: *mut c_void, _err: *mut *const c_char) -> i32 {
        let s = unsafe { &*(ptr as *const XorSession) };
        s.shutdowns.fetch_add(1, Ordering::SeqCst);
        0
    }

    unsafe extern "C" fn xor_free(ptr: *mut c_void) {
        let s = unsafe { Box::from_raw(ptr as *mut XorSession) };
        s.frees.fetch_add(1, Ordering::SeqCst);
        // Drop the box; counters live on the heap allocation until drop returns.
        let _ = s.shutdowns.load(Ordering::SeqCst);
    }

    fn fn_addr(p: *const ()) -> i64 {
        p as usize as i64
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

    fn attach_xor(heap: &mut Heap, sock: TcpStream, xor: u8) -> (Value, *mut XorSession) {
        let stream = alloc_stream(heap, NativeHandle::Tcp(sock), StreamKind::Tcp).expect("alloc");
        let session = XorSession::new(xor);
        let raw = Box::into_raw(session);
        stream_attach(
            heap,
            stream,
            raw as i64,
            fn_addr(xor_read as *const ()),
            fn_addr(xor_write as *const ()),
            fn_addr(xor_shutdown as *const ()),
            fn_addr(xor_free as *const ()),
        )
        .expect("attach");
        (stream, raw)
    }

    #[test]
    fn attach_xor_vtable_is_not_tls_and_hooks_read_write() {
        let _gate = AttachAllowGuard::allow();
        let (client, server) = tcp_pair();
        let mut heap = Heap::default();
        let (stream, ptr) = attach_xor(&mut heap, client, 0x5A);
        assert_eq!(
            with_stream_mut(&mut heap, stream, |s| s.kind).unwrap(),
            StreamKind::Attached
        );
        assert_eq!(
            with_stream_mut(&mut heap, stream, |s| {
                s.attached.as_ref().map(|a| a.session_ptr())
            })
            .unwrap(),
            Some(ptr as *mut c_void)
        );

        let out = make_byte_array(&mut heap, b"hi");
        let n = stream_write(&mut heap, stream, out).expect("write through vtable");
        assert_eq!(n, 2);
        unsafe {
            assert_eq!((*ptr).len.load(Ordering::SeqCst), 2);
            assert_eq!((*ptr).buf[0], b'h' ^ 0x5A);
            assert_eq!((*ptr).buf[1], b'i' ^ 0x5A);
        }

        let buf = make_byte_array(&mut heap, &[0; 8]);
        let got = stream_read(&mut heap, stream, buf).expect("read through vtable");
        assert_eq!(got, Some(2));
        match heap.find_object_by_addr(buf.raw() as u64) {
            Some(Object::Array(arr)) => {
                assert_eq!(arr.as_ref().elements[0].as_int(), b'h' as i64);
                assert_eq!(arr.as_ref().elements[1].as_int(), b'i' as i64);
            }
            _ => panic!("buf"),
        }
        stream_close(&mut heap, stream).ok();
        drop(server);
    }

    #[test]
    fn attach_would_block_parks_via_reactor_wait_fd_no_help() {
        let _gate = AttachAllowGuard::allow();
        let (client, mut server) = tcp_pair();
        let wait = crate::io_handle::WaitHandle::from_tcp(&client);
        let mut heap = Heap::default();
        let (stream, ptr) = attach_xor(&mut heap, client, 0);
        stream_set_read_timeout(&mut heap, stream, 25).expect("timeout");
        unsafe { (*ptr).would_block_reads.store(1, Ordering::SeqCst) };

        let buf = make_byte_array(&mut heap, &[0; 8]);
        let err = stream_read(&mut heap, stream, buf).unwrap_err();
        assert_eq!(err, IoErrorTag::WouldBlock);

        let parked = stream_park(&mut heap, stream);
        assert_eq!(parked, Err(IoErrorTag::TimedOut));

        let parked =
            reactor_wait_fd_no_help(wait, Interest::Readable, Some(Duration::from_millis(25)));
        assert_eq!(parked, Err(IoErrorTag::TimedOut));

        server.write_all(b"x").ok();
        reactor_wait_fd_no_help(wait, Interest::Readable, Some(Duration::from_millis(200)))
            .expect("peer byte should wake readable");

        unsafe {
            (*ptr).buf[0] = b'z';
            (*ptr).len.store(1, Ordering::SeqCst);
        }
        let n = stream_read(&mut heap, stream, buf).expect("retry");
        assert_eq!(n, Some(1));
        stream_close(&mut heap, stream).ok();
    }

    #[test]
    fn attach_write_would_block_sets_park_writable() {
        let _gate = AttachAllowGuard::allow();
        let (client, server) = tcp_pair();
        let mut heap = Heap::default();
        let (stream, ptr) = attach_xor(&mut heap, client, 0);
        unsafe { (*ptr).would_block_writes.store(1, Ordering::SeqCst) };
        let out = make_byte_array(&mut heap, b"ab");
        let err = stream_write(&mut heap, stream, out).unwrap_err();
        assert_eq!(err, IoErrorTag::WouldBlock);
        assert!(
            with_stream_mut(&mut heap, stream, |s| {
                s.attached.as_ref().is_some_and(|a| a.wants_write())
            })
            .unwrap()
        );
        stream_close(&mut heap, stream).ok();
        drop(server);
    }

    struct OrderSession {
        shutdowns: AtomicI32,
        frees: AtomicI32,
        order: AtomicI32,
    }

    unsafe extern "C" fn order_read(
        _p: *mut c_void,
        _b: *mut u8,
        _n: usize,
        _e: *mut *const c_char,
    ) -> i64 {
        0
    }
    unsafe extern "C" fn order_write(
        _p: *mut c_void,
        _b: *const u8,
        n: usize,
        _e: *mut *const c_char,
    ) -> i64 {
        n as i64
    }
    unsafe extern "C" fn order_shutdown(ptr: *mut c_void, _e: *mut *const c_char) -> i32 {
        let s = unsafe { &*(ptr as *const OrderSession) };
        s.shutdowns.fetch_add(1, Ordering::SeqCst);
        s.order
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .ok();
        0
    }
    unsafe extern "C" fn order_free(ptr: *mut c_void) {
        let s = unsafe { Box::from_raw(ptr as *mut OrderSession) };
        s.frees.fetch_add(1, Ordering::SeqCst);
        s.order
            .compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst)
            .ok();
        // Leak the session so the test can read counters after free.
        std::mem::forget(s);
    }

    #[test]
    fn drop_calls_shutdown_then_free_in_order() {
        let _gate = AttachAllowGuard::allow();
        let (client, server) = tcp_pair();
        let mut heap = Heap::default();
        let stream =
            alloc_stream(&mut heap, NativeHandle::Tcp(client), StreamKind::Tcp).expect("alloc");
        let session = Box::new(OrderSession {
            shutdowns: AtomicI32::new(0),
            frees: AtomicI32::new(0),
            order: AtomicI32::new(0),
        });
        let raw = Box::into_raw(session);
        stream_attach(
            &mut heap,
            stream,
            raw as i64,
            fn_addr(order_read as *const ()),
            fn_addr(order_write as *const ()),
            fn_addr(order_shutdown as *const ()),
            fn_addr(order_free as *const ()),
        )
        .expect("attach");
        stream_close(&mut heap, stream).expect("close");
        assert!(with_stream_mut(&mut heap, stream, |s| s.attached.is_none()).unwrap());
        unsafe {
            assert_eq!((*raw).shutdowns.load(Ordering::SeqCst), 1);
            assert_eq!((*raw).frees.load(Ordering::SeqCst), 1);
            assert_eq!((*raw).order.load(Ordering::SeqCst), 2);
            drop(Box::from_raw(raw));
        }
        drop(server);
    }

    #[test]
    fn attach_denied_without_allow_attach() {
        let _gate = AttachAllowGuard::deny();
        let (client, server) = tcp_pair();
        let mut heap = Heap::default();
        let stream =
            alloc_stream(&mut heap, NativeHandle::Tcp(client), StreamKind::Tcp).expect("alloc");
        let err = stream_attach(&mut heap, stream, 1, 2, 3, 4, 5).unwrap_err();
        assert_eq!(err, IoErrorTag::PermissionDenied);
        drop(server);
    }

    #[test]
    fn attach_null_session_is_invalid_when_allowed() {
        let _gate = AttachAllowGuard::allow();
        let (client, server) = tcp_pair();
        let mut heap = Heap::default();
        let stream =
            alloc_stream(&mut heap, NativeHandle::Tcp(client), StreamKind::Tcp).expect("alloc");
        let err = stream_attach(&mut heap, stream, 0, 2, 3, 4, 5).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        drop(server);
    }

    #[test]
    fn attach_rejects_second_attach() {
        let _gate = AttachAllowGuard::allow();
        let (client, server) = tcp_pair();
        let mut heap = Heap::default();
        let (stream, _ptr) = attach_xor(&mut heap, client, 0);
        let err = stream_attach(
            &mut heap,
            stream,
            1,
            fn_addr(xor_read as *const ()),
            fn_addr(xor_write as *const ()),
            fn_addr(xor_shutdown as *const ()),
            fn_addr(xor_free as *const ()),
        )
        .unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, stream).ok();
        drop(server);
    }

    #[test]
    fn stream_fd_i64_marshals_tcp_fd_not_heap_addr() {
        let (client, server) = tcp_pair();
        let expect = {
            #[cfg(unix)]
            {
                use std::os::fd::AsRawFd;
                client.as_raw_fd() as i64
            }
            #[cfg(windows)]
            {
                use std::os::windows::io::AsRawSocket;
                client.as_raw_socket() as i64
            }
        };
        let mut heap = Heap::default();
        let stream =
            alloc_stream(&mut heap, NativeHandle::Tcp(client), StreamKind::Tcp).expect("alloc");
        let fd = crate::io::stream_fd_i64(&heap, stream).expect("fd");
        assert_eq!(fd, expect);
        assert_ne!(fd, stream.as_int());
        drop(server);
    }
}
