//! Host-backed non-blocking IO streams (files, stdio, TCP).
//!
//! Streams are always non-blocking at the OS level. Blocking adapters are
//! Coil userland (coil-stdlib `io/sync.hy`) over L0 + `await_*`. Attached
//! package IO (leftover TLS enable, later others) parks handshake waits on
//! [`reactor_wait_fd_no_help`].

use std::cell::RefCell;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use common::{BUILTIN_IO_ERROR_VARIANTS, BUILTIN_OPTION_VARIANTS, BUILTIN_RESULT_VARIANTS, Value};

use crate::io_handle::{NativeHandle, WaitHandle};
use crate::io_reactor::Interest;
use crate::memory::{Heap, Member, ObjArray, ObjStream, ObjTuple, Object, StreamKind};

type OutputRedirect = *mut (dyn Write + Send);

/// Request to park the VM until a handle is ready (set by `await_*` natives).
#[derive(Debug, Clone)]
pub struct IoParkRequest {
    pub handle: WaitHandle,
    pub interest: Interest,
    pub timeout: Option<Duration>,
}

thread_local! {
    static PENDING_IO_PARK: RefCell<Option<IoParkRequest>> = const { RefCell::new(None) };
    static OUTPUT_REDIRECT: RefCell<Option<OutputRedirect>> = RefCell::new(None);
    static SHARED_PRINT: RefCell<Option<std::sync::Arc<std::sync::Mutex<Vec<u8>>>>> =
        RefCell::new(None);
}

pub(crate) fn take_pending_io_park() -> Option<IoParkRequest> {
    PENDING_IO_PARK.with(|c| c.borrow_mut().take())
}

fn request_io_park(req: IoParkRequest) {
    PENDING_IO_PARK.with(|c| *c.borrow_mut() = Some(req));
}

pub fn set_output_redirect(sink: Option<OutputRedirect>) -> Option<OutputRedirect> {
    OUTPUT_REDIRECT.with(|cell| cell.replace(sink))
}

/// Install the process-wide shared print buffer for this OS thread (workers).
pub fn set_shared_print_redirect(
    buf: Option<std::sync::Arc<std::sync::Mutex<Vec<u8>>>>,
) -> Option<std::sync::Arc<std::sync::Mutex<Vec<u8>>>> {
    SHARED_PRINT.with(|cell| cell.replace(buf))
}

fn with_output_redirect<T>(f: impl FnOnce(&mut (dyn Write + Send)) -> T) -> Option<T> {
    OUTPUT_REDIRECT.with(|cell| {
        let ptr = (*cell.borrow())?;
        Some(f(unsafe { &mut *ptr }))
    })
}

fn write_captured_stdout(bytes: &[u8]) -> Option<Result<usize, IoErrorTag>> {
    if let Some(result) = with_output_redirect(|out| match out.write(bytes) {
        Ok(n) => Ok(n),
        Err(e) => Err(IoErrorTag::from_kind(e.kind())),
    }) {
        return Some(result);
    }
    SHARED_PRINT.with(|cell| {
        let guard = cell.borrow();
        let buf = guard.as_ref()?;
        let mut g = buf.lock().ok()?;
        g.extend_from_slice(bytes);
        Some(Ok(bytes.len()))
    })
}

/// Tag indices for [`IoError`](common::BUILTIN_IO_ERROR_ENUM).
///
/// Append-only — keep discriminants aligned with [`BUILTIN_IO_ERROR_VARIANTS`].
#[repr(u32)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum IoErrorTag {
    WouldBlock = 0,
    NotFound = 1,
    PermissionDenied = 2,
    AlreadyClosed = 3,
    InvalidInput = 4,
    Other = 5,
    NotADirectory = 6,
    AlreadyExists = 7,
    TimedOut = 8,
    Truncated = 9,
    Certificate = 10,
    Handshake = 11,
}

impl IoErrorTag {
    pub fn from_kind(kind: ErrorKind) -> Self {
        match kind {
            ErrorKind::WouldBlock => Self::WouldBlock,
            ErrorKind::TimedOut => Self::TimedOut,
            ErrorKind::NotFound => Self::NotFound,
            ErrorKind::PermissionDenied => Self::PermissionDenied,
            ErrorKind::InvalidInput => Self::InvalidInput,
            ErrorKind::NotADirectory => Self::NotADirectory,
            ErrorKind::AlreadyExists => Self::AlreadyExists,
            ErrorKind::UnexpectedEof => Self::Truncated,
            _ => Self::Other,
        }
    }

    /// Map a `coil_tls_*` `err_out` discriminant (`IoErrorTag` as `i32`).
    pub fn from_abi(code: i32) -> Self {
        match code {
            0 => Self::WouldBlock,
            1 => Self::NotFound,
            2 => Self::PermissionDenied,
            3 => Self::AlreadyClosed,
            4 => Self::InvalidInput,
            5 => Self::Other,
            6 => Self::NotADirectory,
            7 => Self::AlreadyExists,
            8 => Self::TimedOut,
            9 => Self::Truncated,
            10 => Self::Certificate,
            11 => Self::Handshake,
            _ => Self::Other,
        }
    }

    /// Map a `coil_tls_*` `err_out` name (`tls.h`: NUL-terminated tag, NULL = ok).
    pub fn from_abi_name(name: &str) -> Self {
        match name {
            "WouldBlock" => Self::WouldBlock,
            "NotFound" => Self::NotFound,
            "PermissionDenied" => Self::PermissionDenied,
            "AlreadyClosed" => Self::AlreadyClosed,
            "InvalidInput" => Self::InvalidInput,
            "NotADirectory" => Self::NotADirectory,
            "AlreadyExists" => Self::AlreadyExists,
            "TimedOut" => Self::TimedOut,
            "Truncated" => Self::Truncated,
            "Certificate" => Self::Certificate,
            "Handshake" => Self::Handshake,
            _ => Self::Other,
        }
    }
}

/// Allocate `Result::Ok(payload)` on the heap.
pub fn alloc_result_ok(heap: &mut Heap, payload: Value) -> Value {
    let _ = BUILTIN_RESULT_VARIANTS;
    alloc_enum(heap, 0, vec![member_from_value(heap, payload)])
}

/// Allocate `Result::Err(payload)` on the heap.
pub fn alloc_result_err(heap: &mut Heap, payload: Value) -> Value {
    alloc_enum(heap, 1, vec![member_from_value(heap, payload)])
}

/// Allocate `Option::None`.
pub fn alloc_option_none(heap: &mut Heap) -> Value {
    let _ = BUILTIN_OPTION_VARIANTS;
    alloc_enum(heap, 0, vec![])
}

/// Allocate `Option::Some(payload)`.
pub fn alloc_option_some(heap: &mut Heap, payload: Value) -> Value {
    alloc_enum(heap, 1, vec![member_from_value(heap, payload)])
}

/// Allocate a unit-payload `IoError` variant.
pub fn alloc_io_error(heap: &mut Heap, tag: IoErrorTag) -> Value {
    let _ = BUILTIN_IO_ERROR_VARIANTS;
    alloc_enum(heap, tag as u32, vec![])
}

fn alloc_enum(heap: &mut Heap, tag: u32, payload: Vec<Member>) -> Value {
    heap.alloc_enum_value(tag, payload)
}

fn member_from_value(heap: &Heap, value: Value) -> Member {
    if !value.raw().is_null()
        && let Some(obj) = heap.find_object_by_addr(value.raw() as u64)
    {
        Member::Object(obj)
    } else {
        Member::Value(value)
    }
}

/// Convert coil millisecond timeout: `<= 0` clears / means wait forever.
pub fn duration_from_timeout_ms(ms: i64) -> Option<Duration> {
    if ms <= 0 {
        None
    } else {
        Some(Duration::from_millis(ms as u64))
    }
}

fn stream_read_timeout(heap: &mut Heap, stream: Value) -> Result<Option<Duration>, IoErrorTag> {
    with_stream_mut(heap, stream, |s| s.read_timeout)
}

fn stream_write_timeout(heap: &mut Heap, stream: Value) -> Result<Option<Duration>, IoErrorTag> {
    with_stream_mut(heap, stream, |s| s.write_timeout)
}

/// Set soft read deadline for sync adapters (`ms <= 0` clears).
pub fn stream_set_read_timeout(heap: &mut Heap, stream: Value, ms: i64) -> Result<(), IoErrorTag> {
    let d = duration_from_timeout_ms(ms);
    with_stream_mut(heap, stream, |s| -> Result<(), IoErrorTag> {
        if s.closed || s.handle.is_none() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        s.read_timeout = d;
        Ok(())
    })?
}

/// Set soft write deadline for sync adapters (`ms <= 0` clears).
pub fn stream_set_write_timeout(heap: &mut Heap, stream: Value, ms: i64) -> Result<(), IoErrorTag> {
    let d = duration_from_timeout_ms(ms);
    with_stream_mut(heap, stream, |s| -> Result<(), IoErrorTag> {
        if s.closed || s.handle.is_none() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        s.write_timeout = d;
        Ok(())
    })?
}

/// Wait for handle readiness via the VM's [`IoReactor`] (help-steals CPU work when available).
pub fn reactor_wait_fd(
    handle: WaitHandle,
    interest: Interest,
    timeout: Option<Duration>,
) -> Result<(), IoErrorTag> {
    crate::thread::host_io_wait(handle, interest, timeout)
}

/// Wait for handle readiness without CPU help-steal (TLS handshake; see COI-116).
pub fn reactor_wait_fd_no_help(
    handle: WaitHandle,
    interest: Interest,
    timeout: Option<Duration>,
) -> Result<(), IoErrorTag> {
    crate::thread::host_io_wait_no_help(handle, interest, timeout)
}

fn poll_ready(
    handle: WaitHandle,
    for_read: bool,
    timeout: Option<Duration>,
) -> Result<(), IoErrorTag> {
    let interest = if for_read {
        Interest::Readable
    } else {
        Interest::Writable
    };
    reactor_wait_fd(handle, interest, timeout)
}

/// Wrap an owned handle as a heap `Stream` (always non-blocking).
pub fn alloc_stream(heap: &mut Heap, handle: NativeHandle, kind: StreamKind) -> io::Result<Value> {
    handle.set_nonblocking(true)?;
    let (obj, _) = heap.alloc(
        ObjStream {
            handle: Some(handle),
            kind,
            closed: false,
            read_timeout: None,
            write_timeout: None,
            attached: None,
        },
        Object::Stream,
    );
    Ok(Value::from(obj.addr()))
}

pub fn stream_stdin(heap: &mut Heap) -> Result<Value, IoErrorTag> {
    // Dup so closing the Stream does not close process stdin.
    let handle = NativeHandle::dup_stdin().map_err(|e| IoErrorTag::from_kind(e.kind()))?;
    alloc_stream(heap, handle, StreamKind::Stdin).map_err(|e| IoErrorTag::from_kind(e.kind()))
}

pub fn stream_stdout(heap: &mut Heap) -> Result<Value, IoErrorTag> {
    let handle = NativeHandle::dup_stdout().map_err(|e| IoErrorTag::from_kind(e.kind()))?;
    alloc_stream(heap, handle, StreamKind::Stdout).map_err(|e| IoErrorTag::from_kind(e.kind()))
}

pub fn stream_stderr(heap: &mut Heap) -> Result<Value, IoErrorTag> {
    let handle = NativeHandle::dup_stderr().map_err(|e| IoErrorTag::from_kind(e.kind()))?;
    alloc_stream(heap, handle, StreamKind::Stderr).map_err(|e| IoErrorTag::from_kind(e.kind()))
}

pub fn stream_open(heap: &mut Heap, path: &str, mode: &str) -> Result<Value, IoErrorTag> {
    let handle =
        NativeHandle::open_file(path, mode).map_err(|e| IoErrorTag::from_kind(e.kind()))?;
    alloc_stream(heap, handle, StreamKind::File).map_err(|e| IoErrorTag::from_kind(e.kind()))
}

pub(crate) fn peel_one_boxed(heap: &Heap, v: Value) -> Value {
    match heap.find_object_by_addr(v.raw() as u64) {
        Some(Object::Boxed(gc)) => match &gc.as_ref().payload {
            Member::Value(inner) => *inner,
            Member::Object(o) => Value::from(o.addr()),
        },
        _ => v,
    }
}

pub(crate) fn with_stream_mut<R>(
    heap: &mut Heap,
    stream: Value,
    f: impl FnOnce(&mut ObjStream) -> R,
) -> Result<R, IoErrorTag> {
    let stream = peel_one_boxed(heap, stream);
    let addr = stream.raw() as u64;
    let Some(Object::Stream(mut gc)) = heap.find_object_by_addr(addr) else {
        return Err(IoErrorTag::InvalidInput);
    };
    Ok(f(gc.as_mut()))
}

pub fn stream_close(heap: &mut Heap, stream: Value) -> Result<(), IoErrorTag> {
    with_stream_mut(heap, stream, |s| {
        if s.closed {
            return Err(IoErrorTag::AlreadyClosed);
        }
        if let Some(slot) = s.attached.take() {
            slot.shutdown_then_free(s.handle.as_mut());
        }
        s.handle.take();
        s.closed = true;
        Ok(())
    })?
}

/// Non-blocking read into an existing `Vec<byte>` buffer. Returns:
/// - `Ok(Some(n))` bytes written into the buffer
/// - `Ok(None)` EOF
/// - `Err(WouldBlock)` / other
pub fn stream_read(
    heap: &mut Heap,
    stream: Value,
    buf: Value,
) -> Result<Option<usize>, IoErrorTag> {
    let capacity = match heap.find_object_by_addr(buf.raw() as u64) {
        Some(Object::Array(arr_gc)) => arr_gc.as_ref().elements.len(),
        _ => return Err(IoErrorTag::InvalidInput),
    };
    stream_read_into(heap, stream, buf, capacity)
}

/// Read up to `cap` bytes into the prefix of an existing `[byte]` buffer.
fn stream_read_into(
    heap: &mut Heap,
    stream: Value,
    buf: Value,
    cap: usize,
) -> Result<Option<usize>, IoErrorTag> {
    let buf_addr = buf.raw() as u64;
    let capacity = match heap.find_object_by_addr(buf_addr) {
        Some(Object::Array(arr_gc)) => arr_gc.as_ref().elements.len().min(cap),
        _ => return Err(IoErrorTag::InvalidInput),
    };
    if capacity == 0 {
        return Ok(Some(0));
    }
    let mut tmp = vec![0u8; capacity];

    let n = with_stream_mut(heap, stream, |s| -> Result<Option<usize>, IoErrorTag> {
        if s.closed || s.handle.is_none() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        let handle = s.handle.as_mut().unwrap();
        if let Some(att) = s.attached.as_ref() {
            return att.read(&mut tmp);
        }
        match handle.read(&mut tmp) {
            Ok(0) => Ok(None),
            Ok(n) => Ok(Some(n)),
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {
                Err(IoErrorTag::WouldBlock)
            }
            Err(e) => Err(IoErrorTag::from_kind(e.kind())),
        }
    })??;

    if let Some(n) = n {
        let Some(Object::Array(mut arr_gc)) = heap.find_object_by_addr(buf_addr) else {
            return Err(IoErrorTag::InvalidInput);
        };
        let arr: &mut ObjArray = arr_gc.as_mut();
        for i in 0..n {
            arr.elements[i] = Value::from(tmp[i] as i64);
        }
        Ok(Some(n))
    } else {
        Ok(None)
    }
}

pub fn stream_write(heap: &mut Heap, stream: Value, buf: Value) -> Result<usize, IoErrorTag> {
    stream_write_from(heap, stream, buf, 0)
}

/// Non-blocking write of `buf[offset..]`. Avoids allocating a Coil suffix array
/// for partial `write_all` loops (userland sync adapters).
pub fn stream_write_from(
    heap: &mut Heap,
    stream: Value,
    buf: Value,
    offset: i64,
) -> Result<usize, IoErrorTag> {
    if offset < 0 {
        return Err(IoErrorTag::InvalidInput);
    }
    let buf_addr = buf.raw() as u64;
    let bytes: Vec<u8> = match heap.find_object_by_addr(buf_addr) {
        Some(Object::Array(arr_gc)) => {
            let elems = &arr_gc.as_ref().elements;
            let start = offset as usize;
            if start > elems.len() {
                return Err(IoErrorTag::InvalidInput);
            }
            elems[start..]
                .iter()
                .map(|v| {
                    let n = v.as_int();
                    if !(0..=255).contains(&n) { 0 } else { n as u8 }
                })
                .collect()
        }
        _ => return Err(IoErrorTag::InvalidInput),
    };
    stream_write_bytes(heap, stream, &bytes)
}

fn stream_write_bytes(heap: &mut Heap, stream: Value, bytes: &[u8]) -> Result<usize, IoErrorTag> {
    with_stream_mut(heap, stream, |s| -> Result<usize, IoErrorTag> {
        if s.closed || s.handle.is_none() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        if matches!(s.kind, StreamKind::Stdout | StreamKind::Stderr)
            && let Some(result) = write_captured_stdout(bytes)
        {
            return result;
        }
        let handle = s.handle.as_mut().unwrap();
        if let Some(att) = s.attached.as_ref() {
            return att.write(bytes);
        }
        match handle.write(bytes) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {
                Err(IoErrorTag::WouldBlock)
            }
            Err(e) => Err(IoErrorTag::from_kind(e.kind())),
        }
    })?
}

/// Block until `buf.len()` bytes are read, EOF, or a hard error.
pub fn stream_read_exact(
    heap: &mut Heap,
    stream: Value,
    buf: Value,
) -> Result<Option<usize>, IoErrorTag> {
    let buf_addr = buf.raw() as u64;
    let Some(Object::Array(arr_gc)) = heap.find_object_by_addr(buf_addr) else {
        return Err(IoErrorTag::InvalidInput);
    };
    let need = arr_gc.as_ref().elements.len();
    if need == 0 {
        return Ok(Some(0));
    }
    // One reusable scratch for the whole call (no per-iteration Coil alloc).
    let scratch_vals: Vec<Value> = (0..need).map(|_| Value::from(0_i64)).collect();
    let (scratch_obj, _) = heap.alloc(
        ObjArray {
            elements: scratch_vals,
        },
        Object::Array,
    );
    let scratch = Value::from(scratch_obj.addr());
    let mut filled = 0usize;
    while filled < need {
        let remaining = need - filled;
        match stream_read_into(heap, stream, scratch, remaining) {
            Ok(None) => {
                return if filled == 0 {
                    Ok(None)
                } else {
                    Ok(Some(filled))
                };
            }
            Ok(Some(0)) => {
                wait_readable(heap, stream)?;
            }
            Ok(Some(n)) => {
                let Some(Object::Array(src)) = heap.find_object_by_addr(scratch.raw() as u64)
                else {
                    return Err(IoErrorTag::Other);
                };
                let chunk: Vec<Value> = src.as_ref().elements[..n].to_vec();
                let Some(Object::Array(mut dst)) = heap.find_object_by_addr(buf_addr) else {
                    return Err(IoErrorTag::Other);
                };
                for (i, v) in chunk.into_iter().enumerate() {
                    dst.as_mut().elements[filled + i] = v;
                }
                filled += n;
            }
            Err(IoErrorTag::WouldBlock) => {
                wait_readable(heap, stream)?;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(Some(filled))
}

/// Block until EOF; return a new `Vec<byte>` with all data.
pub fn stream_read_to_end(heap: &mut Heap, stream: Value) -> Result<Value, IoErrorTag> {
    let mut acc: Vec<u8> = Vec::new();
    let chunk_size = 4096usize;
    let scratch_vals: Vec<Value> = (0..chunk_size).map(|_| Value::from(0_i64)).collect();
    let (scratch_obj, _) = heap.alloc(
        ObjArray {
            elements: scratch_vals,
        },
        Object::Array,
    );
    let scratch = Value::from(scratch_obj.addr());
    loop {
        match stream_read(heap, stream, scratch) {
            Ok(None) => break,
            Ok(Some(0)) => wait_readable(heap, stream)?,
            Ok(Some(n)) => {
                let Some(Object::Array(src)) = heap.find_object_by_addr(scratch.raw() as u64)
                else {
                    return Err(IoErrorTag::Other);
                };
                for v in &src.as_ref().elements[..n] {
                    acc.push(v.as_int() as u8);
                }
            }
            Err(IoErrorTag::WouldBlock) => wait_readable(heap, stream)?,
            // Unclean TLS close is common; bulk readers treat it as EOF with
            // whatever bytes were already accumulated (L0 `read` still surfaces Truncated).
            Err(IoErrorTag::Truncated) => break,
            Err(e) => return Err(e),
        }
    }
    let elements: Vec<Value> = acc.iter().map(|&b| Value::from(b as i64)).collect();
    let (obj, _) = heap.alloc(ObjArray { elements }, Object::Array);
    Ok(Value::from(obj.addr()))
}

/// Block until the entire buffer is written.
pub fn stream_write_all(heap: &mut Heap, stream: Value, buf: Value) -> Result<(), IoErrorTag> {
    let buf_addr = buf.raw() as u64;
    let Some(Object::Array(arr_gc)) = heap.find_object_by_addr(buf_addr) else {
        return Err(IoErrorTag::InvalidInput);
    };
    let bytes: Vec<u8> = arr_gc
        .as_ref()
        .elements
        .iter()
        .map(|v| v.as_int() as u8)
        .collect();
    let mut offset = 0usize;
    while offset < bytes.len() {
        match stream_write_bytes(heap, stream, &bytes[offset..]) {
            Ok(0) => wait_writable(heap, stream)?,
            Ok(n) => offset += n,
            Err(IoErrorTag::WouldBlock) => wait_writable(heap, stream)?,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn wait_readable(heap: &mut Heap, stream: Value) -> Result<(), IoErrorTag> {
    let timeout = stream_read_timeout(heap, stream)?;
    {
        // Pending attached ciphertext must go out before we can expect a reply.
        // Prefer writable poll so sync adapters (read_to_end after write) progress.
        let wants_write = with_stream_mut(heap, stream, |s| {
            s.attached.as_ref().is_some_and(|t| t.wants_write())
        })?;
        if wants_write {
            let handle = stream_wait_handle(heap, stream)?;
            let write_timeout = stream_write_timeout(heap, stream)?;
            return poll_ready(handle, false, write_timeout);
        }
    }
    let handle = stream_wait_handle(heap, stream)?;
    poll_ready(handle, true, timeout)
}

fn wait_writable(heap: &mut Heap, stream: Value) -> Result<(), IoErrorTag> {
    let timeout = stream_write_timeout(heap, stream)?;
    {
        // Prefer draining pending attached writes when the socket can accept them.
        let wants = with_stream_mut(heap, stream, |s| {
            s.attached.as_ref().is_some_and(|t| t.wants_write())
        })?;
        if wants {
            let handle = stream_wait_handle(heap, stream)?;
            return poll_ready(handle, false, timeout);
        }
    }
    let handle = stream_wait_handle(heap, stream)?;
    poll_ready(handle, false, timeout)
}

/// Non-blocking readiness probe; parks the VM via [`IoParkRequest`] when not ready.
///
/// Returns `Ok(None)` when a park was requested (caller must not push a value).
/// Returns `Ok(Some(Result::Ok(())))` when already ready.
pub fn stream_await_readable(heap: &mut Heap, stream: Value) -> Result<Option<Value>, IoErrorTag> {
    stream_await_interest(heap, stream, Interest::Readable)
}

/// Like [`stream_await_readable`] for writability.
pub fn stream_await_writable(heap: &mut Heap, stream: Value) -> Result<Option<Value>, IoErrorTag> {
    stream_await_interest(heap, stream, Interest::Writable)
}

fn stream_await_interest(
    heap: &mut Heap,
    stream: Value,
    interest: Interest,
) -> Result<Option<Value>, IoErrorTag> {
    let timeout = match interest {
        Interest::Readable => stream_read_timeout(heap, stream)?,
        Interest::Writable => stream_write_timeout(heap, stream)?,
    };
    if interest == Interest::Readable {
        // Attached sessions track last WouldBlock interest; wait writable when
        // handshake / ciphertext still needs to flush (wrong-read park).
        let wants_write = with_stream_mut(heap, stream, |s| {
            s.attached.as_ref().is_some_and(|t| t.wants_write())
        })?;
        if wants_write {
            let handle = stream_wait_handle(heap, stream)?;
            match reactor_wait_fd(handle, Interest::Writable, Some(Duration::ZERO)) {
                Ok(()) => return Ok(Some(as_result_unit(heap, Ok(())))),
                Err(IoErrorTag::TimedOut) => {
                    request_io_park(IoParkRequest {
                        handle,
                        interest: Interest::Writable,
                        timeout: stream_write_timeout(heap, stream)?,
                    });
                    return Ok(None);
                }
                Err(e) => return Ok(Some(as_result_unit(heap, Err(e)))),
            }
        }
    }
    let handle = stream_wait_handle(heap, stream)?;
    // Already ready?
    match reactor_wait_fd(handle, interest, Some(Duration::ZERO)) {
        Ok(()) => Ok(Some(as_result_unit(heap, Ok(())))),
        Err(IoErrorTag::TimedOut) => {
            request_io_park(IoParkRequest {
                handle,
                interest,
                timeout,
            });
            Ok(None)
        }
        Err(e) => Ok(Some(as_result_unit(heap, Err(e)))),
    }
}

/// Drive registered async waiters once (cooperative).
pub fn io_drive(_heap: &mut Heap) -> Value {
    let n = crate::thread::host_io_drive();
    Value::from(n as i64)
}

/// Block until any registered async waiter is ready; returns newly-ready count.
pub fn io_wait_ready(_heap: &mut Heap) -> Value {
    let n = crate::thread::host_io_wait_ready();
    Value::from(n as i64)
}

pub(crate) fn stream_wait_handle(heap: &mut Heap, stream: Value) -> Result<WaitHandle, IoErrorTag> {
    with_stream_mut(heap, stream, |s| {
        if s.closed || s.handle.is_none() {
            Err(IoErrorTag::AlreadyClosed)
        } else {
            Ok(s.handle.as_ref().unwrap().wait_handle())
        }
    })?
}

// ---- TCP ----

pub fn tcp_connect(heap: &mut Heap, host: &str, port: i64) -> Result<Value, IoErrorTag> {
    tcp_connect_timeout(heap, host, port, 0)
}

/// Connect with an optional millisecond deadline (`ms <= 0` waits forever).
pub fn tcp_connect_timeout(
    heap: &mut Heap,
    host: &str,
    port: i64,
    ms: i64,
) -> Result<Value, IoErrorTag> {
    use std::net::ToSocketAddrs;
    if !(0..=65535).contains(&port) {
        return Err(IoErrorTag::InvalidInput);
    }
    let port = port as u16;
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    let addrs: Vec<SocketAddr> = if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
        vec![SocketAddr::new(ip, port)]
    } else {
        (bare, port)
            .to_socket_addrs()
            .map_err(|e| IoErrorTag::from_kind(e.kind()))?
            .collect()
    };
    if addrs.is_empty() {
        return Err(IoErrorTag::NotFound);
    }
    // One absolute deadline across all resolved addresses (not per-addr).
    let deadline = duration_from_timeout_ms(ms).map(|d| Instant::now() + d);
    let mut last_err = IoErrorTag::Other;
    let stream = {
        let mut connected = None;
        for addr in addrs {
            let attempt = match deadline {
                None => TcpStream::connect(addr).map_err(|e| IoErrorTag::from_kind(e.kind())),
                Some(end) => {
                    let now = Instant::now();
                    if now >= end {
                        Err(IoErrorTag::TimedOut)
                    } else {
                        TcpStream::connect_timeout(&addr, end - now).map_err(|e| {
                            if e.kind() == ErrorKind::TimedOut {
                                IoErrorTag::TimedOut
                            } else {
                                IoErrorTag::from_kind(e.kind())
                            }
                        })
                    }
                }
            };
            match attempt {
                Ok(s) => {
                    connected = Some(s);
                    break;
                }
                Err(e) => {
                    last_err = e;
                    if last_err == IoErrorTag::TimedOut {
                        break;
                    }
                }
            }
        }
        connected.ok_or(last_err)?
    };
    stream
        .set_nonblocking(true)
        .map_err(|e| IoErrorTag::from_kind(e.kind()))?;
    alloc_stream(heap, NativeHandle::Tcp(stream), StreamKind::Tcp)
        .map_err(|e| IoErrorTag::from_kind(e.kind()))
}

pub fn tcp_listen(heap: &mut Heap, host: &str, port: i64) -> Result<Value, IoErrorTag> {
    let addr = resolve_socket_addr(host, port)?;
    let listener = TcpListener::bind(addr).map_err(|e| IoErrorTag::from_kind(e.kind()))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| IoErrorTag::from_kind(e.kind()))?;
    alloc_stream(
        heap,
        NativeHandle::Listener(listener),
        StreamKind::TcpListener,
    )
    .map_err(|e| IoErrorTag::from_kind(e.kind()))
}

/// Non-blocking accept. `WouldBlock` if nothing pending.
pub fn tcp_accept(heap: &mut Heap, listener: Value) -> Result<Value, IoErrorTag> {
    let stream = with_stream_mut(heap, listener, |s| -> Result<TcpStream, IoErrorTag> {
        if s.closed || s.handle.is_none() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        if s.kind != StreamKind::TcpListener {
            return Err(IoErrorTag::InvalidInput);
        }
        let listener_sock = s
            .handle
            .as_mut()
            .and_then(NativeHandle::as_listener_mut)
            .ok_or(IoErrorTag::InvalidInput)?;
        match listener_sock.accept() {
            Ok((stream, _)) => Ok(stream),
            Err(e) if e.kind() == ErrorKind::WouldBlock => Err(IoErrorTag::WouldBlock),
            Err(e) => Err(IoErrorTag::from_kind(e.kind())),
        }
    })??;
    stream
        .set_nonblocking(true)
        .map_err(|e| IoErrorTag::from_kind(e.kind()))?;
    alloc_stream(heap, NativeHandle::Tcp(stream), StreamKind::Tcp)
        .map_err(|e| IoErrorTag::from_kind(e.kind()))
}

/// Block until a connection is accepted.
pub fn tcp_accept_wait(heap: &mut Heap, listener: Value) -> Result<Value, IoErrorTag> {
    tcp_accept_wait_timeout(heap, listener, 0)
}

/// Accept with an optional millisecond deadline (`ms <= 0` waits forever).
pub fn tcp_accept_wait_timeout(
    heap: &mut Heap,
    listener: Value,
    ms: i64,
) -> Result<Value, IoErrorTag> {
    let deadline = duration_from_timeout_ms(ms).map(|d| std::time::Instant::now() + d);
    loop {
        match tcp_accept(heap, listener) {
            Ok(s) => return Ok(s),
            Err(IoErrorTag::WouldBlock) => {
                let remaining = match deadline {
                    None => None,
                    Some(end) => {
                        let now = std::time::Instant::now();
                        if now >= end {
                            return Err(IoErrorTag::TimedOut);
                        }
                        Some(end - now)
                    }
                };
                let handle = stream_wait_handle(heap, listener)?;
                poll_ready(handle, true, remaining)?;
            }
            Err(e) => return Err(e),
        }
    }
}

fn format_socket_addr(addr: SocketAddr) -> (String, i64) {
    match addr {
        SocketAddr::V4(v4) => (v4.ip().to_string(), i64::from(v4.port())),
        SocketAddr::V6(v6) => (format!("[{}]", v6.ip()), i64::from(v6.port())),
    }
}

fn tcp_stream_addr(
    heap: &mut Heap,
    stream: Value,
    peer: bool,
) -> Result<(String, i64), IoErrorTag> {
    let kind = with_stream_mut(heap, stream, |s| -> Result<StreamKind, IoErrorTag> {
        if s.closed || s.handle.is_none() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        match s.kind {
            StreamKind::Tcp | StreamKind::TcpListener | StreamKind::Attached => Ok(s.kind),
            _ => Err(IoErrorTag::InvalidInput),
        }
    })??;
    if peer && kind == StreamKind::TcpListener {
        return Err(IoErrorTag::InvalidInput);
    }
    with_stream_mut(heap, stream, |s| -> Result<(String, i64), IoErrorTag> {
        let handle = s.handle.as_mut().ok_or(IoErrorTag::AlreadyClosed)?;
        if kind == StreamKind::TcpListener {
            let sock = handle.as_listener_mut().ok_or(IoErrorTag::InvalidInput)?;
            sock.local_addr()
                .map(format_socket_addr)
                .map_err(|e| IoErrorTag::from_kind(e.kind()))
        } else {
            let sock = handle.as_tcp_mut().ok_or(IoErrorTag::InvalidInput)?;
            let addr = if peer {
                sock.peer_addr()
            } else {
                sock.local_addr()
            };
            addr.map(format_socket_addr)
                .map_err(|e| IoErrorTag::from_kind(e.kind()))
        }
    })?
}

/// Peer `(host, port)` for a connected TCP/TLS stream.
pub fn tcp_peer_addr(heap: &mut Heap, stream: Value) -> Result<Value, IoErrorTag> {
    let (host, port) = tcp_stream_addr(heap, stream, true)?;
    let host_v = {
        let gc = heap.intern(host);
        Value::from(gc.as_ptr() as *mut u8 as u64)
    };
    Ok(alloc_tuple2(heap, host_v, Value::from(port)))
}

/// Local `(host, port)` for a TCP listener or connected TCP/TLS stream.
pub fn tcp_local_addr(heap: &mut Heap, stream: Value) -> Result<Value, IoErrorTag> {
    let (host, port) = tcp_stream_addr(heap, stream, false)?;
    let host_v = {
        let gc = heap.intern(host);
        Value::from(gc.as_ptr() as *mut u8 as u64)
    };
    Ok(alloc_tuple2(heap, host_v, Value::from(port)))
}

/// Enable / disable `TCP_NODELAY` on a TCP or TLS stream.
pub fn tcp_set_nodelay(heap: &mut Heap, stream: Value, enabled: bool) -> Result<(), IoErrorTag> {
    with_stream_mut(heap, stream, |s| -> Result<(), IoErrorTag> {
        if s.closed || s.handle.is_none() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        match s.kind {
            StreamKind::Tcp | StreamKind::Attached => {}
            _ => return Err(IoErrorTag::InvalidInput),
        }
        let sock = s
            .handle
            .as_mut()
            .and_then(NativeHandle::as_tcp_mut)
            .ok_or(IoErrorTag::InvalidInput)?;
        sock.set_nodelay(enabled)
            .map_err(|e| IoErrorTag::from_kind(e.kind()))
    })?
}

/// Half-close a TCP/TLS stream. `how`: `0` read, `1` write, `2` both.
pub fn tcp_shutdown(heap: &mut Heap, stream: Value, how: i64) -> Result<(), IoErrorTag> {
    use std::net::Shutdown;
    let mode = match how {
        0 => Shutdown::Read,
        1 => Shutdown::Write,
        2 => Shutdown::Both,
        _ => return Err(IoErrorTag::InvalidInput),
    };
    with_stream_mut(heap, stream, |s| -> Result<(), IoErrorTag> {
        if s.closed || s.handle.is_none() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        match s.kind {
            StreamKind::Tcp | StreamKind::Attached => {}
            _ => return Err(IoErrorTag::InvalidInput),
        }
        let sock = s
            .handle
            .as_mut()
            .and_then(NativeHandle::as_tcp_mut)
            .ok_or(IoErrorTag::InvalidInput)?;
        sock.shutdown(mode)
            .map_err(|e| IoErrorTag::from_kind(e.kind()))
    })?
}

// ---- UDP ----

fn alloc_tuple2(heap: &mut Heap, a: Value, b: Value) -> Value {
    let (obj, _) = heap.alloc(
        ObjTuple {
            elements: vec![a, b],
        },
        Object::Tuple,
    );
    Value::from(obj.addr())
}

fn alloc_tuple3(heap: &mut Heap, a: Value, b: Value, c: Value) -> Value {
    let (obj, _) = heap.alloc(
        ObjTuple {
            elements: vec![a, b, c],
        },
        Object::Tuple,
    );
    Value::from(obj.addr())
}

/// Resolve host/port to a single [`SocketAddr`] (IPv4, bracketed/bare IPv6, or DNS).
fn resolve_socket_addr(host: &str, port: i64) -> Result<SocketAddr, IoErrorTag> {
    use std::net::{IpAddr, ToSocketAddrs};
    if !(0..=65535).contains(&port) {
        return Err(IoErrorTag::InvalidInput);
    }
    let port = port as u16;
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, port));
    }
    let mut addrs = (bare, port)
        .to_socket_addrs()
        .map_err(|e| IoErrorTag::from_kind(e.kind()))?;
    addrs.next().ok_or(IoErrorTag::NotFound)
}

fn parse_socket_addr(host: &str, port: i64) -> Result<SocketAddr, IoErrorTag> {
    resolve_socket_addr(host, port)
}

/// Bind a non-blocking UDP socket. `port` may be `0` (ephemeral);
/// use [`udp_local_port`] to read the assigned port.
pub fn udp_bind(heap: &mut Heap, host: &str, port: i64) -> Result<Value, IoErrorTag> {
    let addr = parse_socket_addr(host, port)?;
    let sock = UdpSocket::bind(addr).map_err(|e| IoErrorTag::from_kind(e.kind()))?;
    sock.set_nonblocking(true)
        .map_err(|e| IoErrorTag::from_kind(e.kind()))?;
    alloc_stream(heap, NativeHandle::Udp(sock), StreamKind::Udp)
        .map_err(|e| IoErrorTag::from_kind(e.kind()))
}

/// Create a connected non-blocking UDP socket toward `(host, port)`.
///
/// After connect, [`stream_read`] / [`stream_write`] (and the sync adapters)
/// work against that peer. Unconnected peers still use
/// [`udp_send_to`] / [`udp_recv_from`].
pub fn udp_connect(heap: &mut Heap, host: &str, port: i64) -> Result<Value, IoErrorTag> {
    let peer = parse_socket_addr(host, port)?;
    // Ephemeral local bind, then connect for a default peer.
    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| IoErrorTag::from_kind(e.kind()))?;
    sock.connect(peer)
        .map_err(|e| IoErrorTag::from_kind(e.kind()))?;
    sock.set_nonblocking(true)
        .map_err(|e| IoErrorTag::from_kind(e.kind()))?;
    alloc_stream(heap, NativeHandle::Udp(sock), StreamKind::Udp)
        .map_err(|e| IoErrorTag::from_kind(e.kind()))
}

/// Local UDP port (after bind / connect).
pub fn udp_local_port(heap: &mut Heap, stream: Value) -> Result<i64, IoErrorTag> {
    with_stream_mut(heap, stream, |s| -> Result<i64, IoErrorTag> {
        let sock = s
            .handle
            .as_mut()
            .and_then(NativeHandle::as_udp_mut)
            .ok_or(IoErrorTag::AlreadyClosed)?;
        sock.local_addr()
            .map(|a| a.port() as i64)
            .map_err(|e| IoErrorTag::from_kind(e.kind()))
    })?
}

/// Non-blocking `sendto`. Returns bytes sent.
pub fn udp_send_to(
    heap: &mut Heap,
    stream: Value,
    buf: Value,
    host: &str,
    port: i64,
) -> Result<usize, IoErrorTag> {
    let peer = parse_socket_addr(host, port)?;
    let bytes = value_as_bytes(heap, buf)?;
    with_stream_mut(heap, stream, |s| -> Result<usize, IoErrorTag> {
        let sock = s
            .handle
            .as_mut()
            .and_then(NativeHandle::as_udp_mut)
            .ok_or(IoErrorTag::AlreadyClosed)?;
        match sock.send_to(&bytes, peer) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {
                Err(IoErrorTag::WouldBlock)
            }
            Err(e) => Err(IoErrorTag::from_kind(e.kind())),
        }
    })?
}

/// Non-blocking `recvfrom` into `buf`.
///
/// On success returns a heap tuple `(nbytes: int, peer_host: string, peer_port: int)`.
/// The first `nbytes` elements of `buf` are filled.
pub fn udp_recv_from(heap: &mut Heap, stream: Value, buf: Value) -> Result<Value, IoErrorTag> {
    let buf_addr = buf.raw() as u64;
    let capacity = match heap.find_object_by_addr(buf_addr) {
        Some(Object::Array(arr_gc)) => arr_gc.as_ref().elements.len(),
        _ => return Err(IoErrorTag::InvalidInput),
    };
    if capacity == 0 {
        let host = {
            let gc = heap.intern(String::new());
            Value::from(gc.as_ptr() as *mut u8 as u64)
        };
        return Ok(alloc_tuple3(
            heap,
            Value::from(0_i64),
            host,
            Value::from(0_i64),
        ));
    }
    let mut tmp = vec![0u8; capacity];

    let (n, peer) = with_stream_mut(
        heap,
        stream,
        |s| -> Result<(usize, SocketAddr), IoErrorTag> {
            let sock = s
                .handle
                .as_mut()
                .and_then(NativeHandle::as_udp_mut)
                .ok_or(IoErrorTag::AlreadyClosed)?;
            match sock.recv_from(&mut tmp) {
                Ok((n, peer)) => Ok((n, peer)),
                Err(e)
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted =>
                {
                    Err(IoErrorTag::WouldBlock)
                }
                Err(e) => Err(IoErrorTag::from_kind(e.kind())),
            }
        },
    )??;

    {
        let Some(Object::Array(mut arr_gc)) = heap.find_object_by_addr(buf_addr) else {
            return Err(IoErrorTag::InvalidInput);
        };
        let arr: &mut ObjArray = arr_gc.as_mut();
        for i in 0..n {
            arr.elements[i] = Value::from(tmp[i] as i64);
        }
    }

    let host_str = match peer {
        SocketAddr::V4(a) => a.ip().to_string(),
        SocketAddr::V6(a) => a.ip().to_string(),
    };
    let host = {
        let gc = heap.intern(host_str);
        Value::from(gc.as_ptr() as *mut u8 as u64)
    };
    Ok(alloc_tuple3(
        heap,
        Value::from(n as i64),
        host,
        Value::from(peer.port() as i64),
    ))
}

/// Block until a datagram arrives, then [`udp_recv_from`].
pub fn udp_recv_from_wait(heap: &mut Heap, stream: Value, buf: Value) -> Result<Value, IoErrorTag> {
    loop {
        match udp_recv_from(heap, stream, buf) {
            Err(IoErrorTag::WouldBlock) => wait_readable(heap, stream)?,
            other => return other,
        }
    }
}

/// Decode a heap string `Value` into a Rust `String`.
pub fn value_as_string(heap: &Heap, v: Value) -> Result<String, IoErrorTag> {
    let v = peel_one_boxed(heap, v);
    match heap.find_object_by_addr(v.raw() as u64) {
        Some(Object::String(gc)) => Ok(gc.as_ref().data.clone()),
        _ => Err(IoErrorTag::InvalidInput),
    }
}

/// Marshal a Stream (or one Boxed Stream) to its fd. `None` if `v` is not a stream.
pub fn stream_fd_i64(heap: &Heap, v: Value) -> Option<i64> {
    let v = peel_one_boxed(heap, v);
    match heap.find_object_by_addr(v.raw() as u64) {
        Some(Object::Stream(gc)) => gc.as_ref().handle.as_ref().map(|h| h.fd_i64()),
        _ => None,
    }
}

/// Read a heap `Vec<byte>` (Array carrier) into a Rust `Vec<u8>`.
pub fn value_as_bytes(heap: &Heap, v: Value) -> Result<Vec<u8>, IoErrorTag> {
    match heap.find_object_by_addr(v.raw() as u64) {
        Some(Object::Array(arr_gc)) => Ok(arr_gc
            .as_ref()
            .elements
            .iter()
            .map(|e| {
                let n = e.as_int();
                if (0..=255).contains(&n) {
                    n as u8
                } else {
                    // Out-of-range elements are a typechecker bug; clamp
                    // defensively rather than panicking in the host.
                    n as u8
                }
            })
            .collect()),
        _ => Err(IoErrorTag::InvalidInput),
    }
}

/// Decode `Vec<byte>` as UTF-8 into a heap string.
///
/// Invalid UTF-8 → `Err(InvalidInput)`.
pub fn from_bytes(heap: &mut Heap, buf: Value) -> Result<Value, IoErrorTag> {
    let bytes = value_as_bytes(heap, buf)?;
    let s = String::from_utf8(bytes).map_err(|_| IoErrorTag::InvalidInput)?;
    let gc = heap.intern(s);
    Ok(Value::from(gc.as_ptr() as *mut u8 as u64))
}

/// Encode a heap string as a fresh `Vec<byte>` (UTF-8).
///
/// Non-string input yields an empty buffer (defensive — the typechecker
/// rejects this case).
pub fn to_bytes(heap: &mut Heap, s: Value) -> Value {
    let bytes = match value_as_string(heap, s) {
        Ok(text) => text.into_bytes(),
        Err(_) => Vec::new(),
    };
    let elements: Vec<Value> = bytes.iter().map(|&b| Value::from(b as i64)).collect();
    let (obj, _) = heap.alloc(ObjArray { elements }, Object::Array);
    Value::from(obj.addr())
}

/// Helper: wrap a fallible stream op that returns a Value into `Result<_, IoError>`.
pub fn as_result_value(heap: &mut Heap, r: Result<Value, IoErrorTag>) -> Value {
    match r {
        Ok(v) => alloc_result_ok(heap, v),
        Err(tag) => {
            let err = alloc_io_error(heap, tag);
            alloc_result_err(heap, err)
        }
    }
}

/// Helper: `Result<Option<int>, IoError>` encoding.
pub fn as_result_option_int(heap: &mut Heap, r: Result<Option<usize>, IoErrorTag>) -> Value {
    match r {
        Ok(None) => {
            let none = alloc_option_none(heap);
            alloc_result_ok(heap, none)
        }
        Ok(Some(n)) => {
            let some = alloc_option_some(heap, Value::from(n as i64));
            alloc_result_ok(heap, some)
        }
        Err(tag) => {
            let err = alloc_io_error(heap, tag);
            alloc_result_err(heap, err)
        }
    }
}

/// Helper: `Result<int, IoError>`.
pub fn as_result_int(heap: &mut Heap, r: Result<usize, IoErrorTag>) -> Value {
    match r {
        Ok(n) => alloc_result_ok(heap, Value::from(n as i64)),
        Err(tag) => {
            let err = alloc_io_error(heap, tag);
            alloc_result_err(heap, err)
        }
    }
}

/// Helper: `Result<(), IoError>` — Ok payload is unit (null/default).
pub fn as_result_unit(heap: &mut Heap, r: Result<(), IoErrorTag>) -> Value {
    match r {
        Ok(()) => alloc_result_ok(heap, Value::default()),
        Err(tag) => {
            let err = alloc_io_error(heap, tag);
            alloc_result_err(heap, err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Heap;
    use std::io::{Read as IoRead, Write as IoWrite};
    use std::net::{TcpListener, TcpStream};
    use std::thread;
    use std::time::Duration;

    fn enum_tag(heap: &Heap, v: Value) -> Option<u32> {
        match heap.find_object_by_addr(v.raw() as u64) {
            Some(Object::Enum(gc)) => Some(gc.as_ref().tag),
            _ => None,
        }
    }

    fn make_byte_array(heap: &mut Heap, bytes: &[u8]) -> Value {
        let elements: Vec<Value> = bytes.iter().map(|&b| Value::from(b as i64)).collect();
        let (obj, _) = heap.alloc(ObjArray { elements }, Object::Array);
        Value::from(obj.addr())
    }

    fn array_bytes(heap: &Heap, v: Value) -> Vec<u8> {
        match heap.find_object_by_addr(v.raw() as u64) {
            Some(Object::Array(gc)) => gc
                .as_ref()
                .elements
                .iter()
                .map(|e| e.as_int() as u8)
                .collect(),
            _ => panic!("expected array"),
        }
    }

    #[test]
    fn file_round_trip_read_to_end() {
        let path = std::env::temp_dir().join("coil_io_unit_roundtrip.bin");
        let mut heap = Heap::default();
        let data = make_byte_array(&mut heap, b"Hi");
        let w = stream_open(&mut heap, path.to_str().unwrap(), "w").expect("open w");
        stream_write_all(&mut heap, w, data).expect("write_all");
        stream_close(&mut heap, w).expect("close w");

        let r = stream_open(&mut heap, path.to_str().unwrap(), "r").expect("open r");
        let buf = stream_read_to_end(&mut heap, r).expect("read_to_end");
        stream_close(&mut heap, r).expect("close r");
        assert_eq!(array_bytes(&heap, buf), b"Hi");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn stream_open_rejects_invalid_mode() {
        let mut heap = Heap::default();
        let path = std::env::temp_dir().join("coil_io_bad_mode.bin");
        let err = stream_open(&mut heap, path.to_str().unwrap(), "xx").unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
    }

    #[test]
    fn stream_open_append_preserves_bytes() {
        let path = std::env::temp_dir().join("coil_io_append_mode.bin");
        let _ = std::fs::remove_file(&path);
        let mut heap = Heap::default();
        let first = make_byte_array(&mut heap, b"ab");
        let second = make_byte_array(&mut heap, b"cd");
        let w = stream_open(&mut heap, path.to_str().unwrap(), "w").expect("open w");
        stream_write_all(&mut heap, w, first).expect("write");
        stream_close(&mut heap, w).expect("close w");
        let a = stream_open(&mut heap, path.to_str().unwrap(), "a").expect("open a");
        stream_write_all(&mut heap, a, second).expect("append");
        stream_close(&mut heap, a).expect("close a");
        let r = stream_open(&mut heap, path.to_str().unwrap(), "r").expect("open r");
        let buf = stream_read_to_end(&mut heap, r).expect("read");
        stream_close(&mut heap, r).expect("close r");
        assert_eq!(array_bytes(&heap, buf), b"abcd");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_from_rejects_negative_and_past_end_offset() {
        let path = std::env::temp_dir().join("coil_io_write_from_bad.bin");
        let mut heap = Heap::default();
        let buf = make_byte_array(&mut heap, b"abcd");
        let w = stream_open(&mut heap, path.to_str().unwrap(), "w").expect("open");
        assert_eq!(
            stream_write_from(&mut heap, w, buf, -1).unwrap_err(),
            IoErrorTag::InvalidInput
        );
        assert_eq!(
            stream_write_from(&mut heap, w, buf, 5).unwrap_err(),
            IoErrorTag::InvalidInput
        );
        stream_close(&mut heap, w).unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_from_at_len_is_empty_write() {
        let path = std::env::temp_dir().join("coil_io_write_from_at_len.bin");
        let mut heap = Heap::default();
        let buf = make_byte_array(&mut heap, b"xyz");
        let w = stream_open(&mut heap, path.to_str().unwrap(), "w").expect("open");
        assert_eq!(stream_write_from(&mut heap, w, buf, 3).expect("write"), 0);
        stream_close(&mut heap, w).unwrap();
        let r = stream_open(&mut heap, path.to_str().unwrap(), "r").expect("open r");
        let got = stream_read_to_end(&mut heap, r).expect("read");
        stream_close(&mut heap, r).unwrap();
        assert_eq!(array_bytes(&heap, got), b"");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_from_mid_offset_writes_suffix() {
        let path = std::env::temp_dir().join("coil_io_write_from_mid.bin");
        let mut heap = Heap::default();
        let buf = make_byte_array(&mut heap, b"XXXhello");
        let w = stream_open(&mut heap, path.to_str().unwrap(), "w").expect("open");
        let n = stream_write_from(&mut heap, w, buf, 3).expect("write_from");
        assert_eq!(n, 5);
        stream_close(&mut heap, w).unwrap();
        let r = stream_open(&mut heap, path.to_str().unwrap(), "r").expect("open r");
        let got = stream_read_to_end(&mut heap, r).expect("read");
        stream_close(&mut heap, r).unwrap();
        assert_eq!(array_bytes(&heap, got), b"hello");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_exact_empty_buffer_returns_zero() {
        let path = std::env::temp_dir().join("coil_io_read_exact_empty.bin");
        {
            let _f = std::fs::File::create(&path).unwrap();
        }
        let mut heap = Heap::default();
        let s = stream_open(&mut heap, path.to_str().unwrap(), "r").expect("open");
        let buf = make_byte_array(&mut heap, &[]);
        assert_eq!(
            stream_read_exact(&mut heap, s, buf).expect("read_exact"),
            Some(0)
        );
        stream_close(&mut heap, s).unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_file_read_returns_eof_none() {
        let path = std::env::temp_dir().join("coil_io_unit_eof.bin");
        {
            let _f = std::fs::File::create(&path).unwrap();
        }
        let mut heap = Heap::default();
        let s = stream_open(&mut heap, path.to_str().unwrap(), "r").expect("open");
        let buf = make_byte_array(&mut heap, &[0, 0, 0, 0]);
        let r = stream_read(&mut heap, s, buf).expect("read");
        assert_eq!(r, None);
        stream_close(&mut heap, s).unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn close_then_read_is_already_closed() {
        let path = std::env::temp_dir().join("coil_io_unit_closed.bin");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(b"x").unwrap();
        }
        let mut heap = Heap::default();
        let s = stream_open(&mut heap, path.to_str().unwrap(), "r").unwrap();
        stream_close(&mut heap, s).unwrap();
        let buf = make_byte_array(&mut heap, &[0]);
        let err = stream_read(&mut heap, s, buf).unwrap_err();
        assert_eq!(err, IoErrorTag::AlreadyClosed);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn result_helpers_encode_ok_none_and_would_block() {
        let mut heap = Heap::default();
        let ok_none = as_result_option_int(&mut heap, Ok(None));
        assert_eq!(enum_tag(&heap, ok_none), Some(0));
        let err = as_result_option_int(&mut heap, Err(IoErrorTag::WouldBlock));
        assert_eq!(enum_tag(&heap, err), Some(1));
    }

    #[test]
    fn from_bytes_decodes_utf8() {
        let mut heap = Heap::default();
        let buf = make_byte_array(&mut heap, b"hello");
        let s = from_bytes(&mut heap, buf).expect("utf-8");
        assert_eq!(value_as_string(&heap, s).unwrap(), "hello");
    }

    #[test]
    fn from_bytes_rejects_invalid_utf8() {
        let mut heap = Heap::default();
        let buf = make_byte_array(&mut heap, &[0xff, 0xfe]);
        let err = from_bytes(&mut heap, buf).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
    }

    #[test]
    fn to_bytes_round_trips_with_from_bytes() {
        let mut heap = Heap::default();
        let s = {
            let gc = heap.intern("Hi".into());
            Value::from(gc.as_ptr() as *mut u8 as u64)
        };
        let buf = to_bytes(&mut heap, s);
        assert_eq!(array_bytes(&heap, buf), b"Hi");
        let back = from_bytes(&mut heap, buf).expect("round-trip");
        assert_eq!(value_as_string(&heap, back).unwrap(), "Hi");
    }

    #[test]
    fn from_bytes_as_result_wraps_ok_and_err() {
        let mut heap = Heap::default();
        let ok_buf = make_byte_array(&mut heap, b"x");
        let ok_inner = from_bytes(&mut heap, ok_buf);
        let ok = as_result_value(&mut heap, ok_inner);
        assert_eq!(enum_tag(&heap, ok), Some(0));

        let err_buf = make_byte_array(&mut heap, &[0x80]);
        let err_inner = from_bytes(&mut heap, err_buf);
        let err = as_result_value(&mut heap, err_inner);
        assert_eq!(enum_tag(&heap, err), Some(1));
    }

    fn tuple_elems(heap: &Heap, v: Value) -> Vec<Value> {
        match heap.find_object_by_addr(v.raw() as u64) {
            Some(Object::Tuple(gc)) => gc.as_ref().elements.clone(),
            _ => panic!("expected tuple"),
        }
    }

    #[test]
    fn udp_bind_send_to_recv_from_round_trip() {
        let mut heap = Heap::default();
        let server = udp_bind(&mut heap, "127.0.0.1", 0).expect("bind server");
        let port = udp_local_port(&mut heap, server).expect("local port");
        assert!(port > 0);

        let client = udp_bind(&mut heap, "127.0.0.1", 0).expect("bind client");
        let msg = make_byte_array(&mut heap, b"Hi");
        let n = udp_send_to(&mut heap, client, msg, "127.0.0.1", port).expect("send_to");
        assert_eq!(n, 2);

        let buf = make_byte_array(&mut heap, &[0, 0, 0, 0, 0, 0, 0, 0]);
        let t = udp_recv_from_wait(&mut heap, server, buf).expect("recv");
        let elems = tuple_elems(&heap, t);
        assert_eq!(elems[0].as_int(), 2);
        assert_eq!(
            elems[2].as_int(),
            udp_local_port(&mut heap, client).unwrap()
        );
        assert_eq!(&array_bytes(&heap, buf)[..2], b"Hi");

        stream_close(&mut heap, server).unwrap();
        stream_close(&mut heap, client).unwrap();
    }

    #[test]
    fn udp_connect_write_read_round_trip() {
        let mut heap = Heap::default();
        let server = udp_bind(&mut heap, "127.0.0.1", 0).expect("bind server");
        let port = udp_local_port(&mut heap, server).expect("port");
        let client = udp_connect(&mut heap, "127.0.0.1", port).expect("connect");

        let msg = make_byte_array(&mut heap, b"Yo");
        stream_write_all(&mut heap, client, msg).expect("write_all");

        let buf = make_byte_array(&mut heap, &[0, 0, 0, 0]);
        let t = udp_recv_from_wait(&mut heap, server, buf).expect("recv");
        assert_eq!(tuple_elems(&heap, t)[0].as_int(), 2);
        assert_eq!(&array_bytes(&heap, buf)[..2], b"Yo");

        stream_close(&mut heap, server).unwrap();
        stream_close(&mut heap, client).unwrap();
    }

    #[test]
    fn tcp_listen_accept_echo_localhost() {
        let mut heap = Heap::default();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port() as u16;
        listener.set_nonblocking(true).unwrap();
        let listen_stream = alloc_stream(
            &mut heap,
            NativeHandle::Listener(listener),
            StreamKind::TcpListener,
        )
        .unwrap();

        let client = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(b"ping").unwrap();
            let mut buf = [0u8; 4];
            s.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"ping");
        });

        let conn = tcp_accept_wait(&mut heap, listen_stream).expect("accept");
        let buf = make_byte_array(&mut heap, &[0, 0, 0, 0]);
        let n = stream_read_exact(&mut heap, conn, buf).expect("read_exact");
        assert_eq!(n, Some(4));
        assert_eq!(&array_bytes(&heap, buf)[..4], b"ping");
        let reply = make_byte_array(&mut heap, b"ping");
        stream_write_all(&mut heap, conn, reply).unwrap();
        stream_close(&mut heap, conn).unwrap();
        client.join().unwrap();
        stream_close(&mut heap, listen_stream).unwrap();
    }

    #[test]
    fn tcp_listen_accept_write_all_twice() {
        let mut heap = Heap::default();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let listen_stream = alloc_stream(
            &mut heap,
            NativeHandle::Listener(listener),
            StreamKind::TcpListener,
        )
        .unwrap();
        let reply = make_byte_array(&mut heap, b"ok");

        for round in 0..2 {
            let client = thread::spawn(move || {
                thread::sleep(Duration::from_millis(20));
                let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
                let mut buf = [0u8; 2];
                s.read_exact(&mut buf).unwrap();
                assert_eq!(&buf, b"ok");
            });

            let conn = tcp_accept_wait(&mut heap, listen_stream).expect("accept");
            stream_write_all(&mut heap, conn, reply).expect("write_all");
            stream_close(&mut heap, conn).unwrap();
            client.join().unwrap();
        }

        stream_close(&mut heap, listen_stream).unwrap();
    }

    #[test]
    fn tcp_connect_timeout_refused_is_error() {
        let mut heap = Heap::default();
        // Port 1 is almost never listening; short timeout still fails fast.
        let err = tcp_connect_timeout(&mut heap, "127.0.0.1", 1, 50).unwrap_err();
        assert!(
            matches!(
                err,
                IoErrorTag::Other | IoErrorTag::TimedOut | IoErrorTag::PermissionDenied
            ),
            "unexpected {err:?}"
        );
    }

    #[test]
    fn tcp_accept_wait_timeout_times_out() {
        let mut heap = Heap::default();
        let listener = tcp_listen(&mut heap, "127.0.0.1", 0).expect("listen");
        let err = tcp_accept_wait_timeout(&mut heap, listener, 30).unwrap_err();
        assert_eq!(err, IoErrorTag::TimedOut);
        stream_close(&mut heap, listener).unwrap();
    }

    #[test]
    fn tcp_peer_local_addr_and_nodelay() {
        let mut heap = Heap::default();
        let listener = tcp_listen(&mut heap, "127.0.0.1", 0).expect("listen");
        let local = tcp_local_addr(&mut heap, listener).expect("local");
        let local_elems = tuple_elems(&heap, local);
        assert_eq!(
            value_as_string(&heap, local_elems[0]).expect("host"),
            "127.0.0.1"
        );
        let port = local_elems[1].as_int();
        assert!(port > 0);

        // Hold the peer socket open until after shutdown — a sleep+drop races
        // on macOS and makes `shutdown(Both)` return Other.
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let client = thread::spawn(move || {
            let stream = TcpStream::connect(("127.0.0.1", port as u16)).ok();
            let _ = release_rx.recv();
            drop(stream);
        });
        let conn = tcp_accept_wait_timeout(&mut heap, listener, 2000).expect("accept");
        tcp_set_nodelay(&mut heap, conn, true).expect("nodelay");
        let peer = tcp_peer_addr(&mut heap, conn).expect("peer");
        let peer_elems = tuple_elems(&heap, peer);
        assert!(peer_elems[1].as_int() > 0);
        tcp_shutdown(&mut heap, conn, 2).expect("shutdown both");
        let _ = release_tx.send(());
        stream_close(&mut heap, conn).ok();
        stream_close(&mut heap, listener).ok();
        let _ = client.join();
    }

    #[test]
    fn stream_timeouts_round_trip_setters() {
        let mut heap = Heap::default();
        let path = "coil_io_timeout_setters.bin";
        let _ = std::fs::remove_file(path);
        let s = stream_open(&mut heap, path, "w").expect("open");
        stream_set_read_timeout(&mut heap, s, 100).expect("read to");
        stream_set_write_timeout(&mut heap, s, 200).expect("write to");
        stream_set_read_timeout(&mut heap, s, 0).expect("clear read");
        stream_set_write_timeout(&mut heap, s, -1).expect("clear write");
        stream_close(&mut heap, s).unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn io_error_from_kind_keeps_timed_out_and_truncated() {
        assert_eq!(
            IoErrorTag::from_kind(ErrorKind::TimedOut),
            IoErrorTag::TimedOut
        );
        assert_eq!(
            IoErrorTag::from_kind(ErrorKind::UnexpectedEof),
            IoErrorTag::Truncated
        );
        assert_eq!(
            IoErrorTag::from_kind(ErrorKind::WouldBlock),
            IoErrorTag::WouldBlock
        );
        assert_ne!(
            IoErrorTag::from_kind(ErrorKind::TimedOut),
            IoErrorTag::WouldBlock
        );
    }

    #[test]
    fn duration_from_timeout_ms_clears_non_positive() {
        assert_eq!(duration_from_timeout_ms(0), None);
        assert_eq!(duration_from_timeout_ms(-5), None);
        assert_eq!(
            duration_from_timeout_ms(25),
            Some(Duration::from_millis(25))
        );
    }

    #[test]
    fn stream_timeouts_on_closed_are_already_closed() {
        let mut heap = Heap::default();
        let path = "coil_io_timeout_closed.bin";
        let _ = std::fs::remove_file(path);
        let s = stream_open(&mut heap, path, "w").expect("open");
        stream_close(&mut heap, s).unwrap();
        assert_eq!(
            stream_set_read_timeout(&mut heap, s, 10).unwrap_err(),
            IoErrorTag::AlreadyClosed
        );
        assert_eq!(
            stream_set_write_timeout(&mut heap, s, 10).unwrap_err(),
            IoErrorTag::AlreadyClosed
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn tcp_shutdown_rejects_invalid_how() {
        let mut heap = Heap::default();
        let listener = tcp_listen(&mut heap, "127.0.0.1", 0).expect("listen");
        let port = {
            let local = tcp_local_addr(&mut heap, listener).expect("local");
            tuple_elems(&heap, local)[1].as_int()
        };
        let client = thread::spawn(move || {
            let _ = TcpStream::connect(("127.0.0.1", port as u16));
            thread::sleep(Duration::from_millis(200));
        });
        let conn = tcp_accept_wait_timeout(&mut heap, listener, 2000).expect("accept");
        assert_eq!(
            tcp_shutdown(&mut heap, conn, 3).unwrap_err(),
            IoErrorTag::InvalidInput
        );
        assert_eq!(
            tcp_shutdown(&mut heap, conn, -1).unwrap_err(),
            IoErrorTag::InvalidInput
        );
        stream_close(&mut heap, conn).ok();
        stream_close(&mut heap, listener).ok();
        let _ = client.join();
    }

    #[test]
    fn tcp_read_exact_honors_read_timeout() {
        let mut heap = Heap::default();
        let listener = tcp_listen(&mut heap, "127.0.0.1", 0).expect("listen");
        let port = {
            let local = tcp_local_addr(&mut heap, listener).expect("local");
            tuple_elems(&heap, local)[1].as_int()
        };
        let peer = thread::spawn(move || {
            let sock = TcpStream::connect(("127.0.0.1", port as u16)).expect("connect");
            // Hold the connection open without sending so the peer times out.
            thread::sleep(Duration::from_millis(400));
            drop(sock);
        });
        let conn = tcp_accept_wait_timeout(&mut heap, listener, 2000).expect("accept");
        stream_set_read_timeout(&mut heap, conn, 40).expect("set timeout");
        let buf = make_byte_array(&mut heap, &[0u8; 4]);
        let err = stream_read_exact(&mut heap, conn, buf).unwrap_err();
        assert_eq!(err, IoErrorTag::TimedOut);
        stream_close(&mut heap, conn).ok();
        stream_close(&mut heap, listener).ok();
        let _ = peer.join();
    }

    #[test]
    fn tcp_connect_localhost_tries_all_resolved_addrs() {
        // Bind IPv4-only; `localhost` may resolve `::1` first — connect must
        // still succeed by trying every resolved address.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port() as i64;
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let client = tcp_connect(&mut heap, "localhost", port).expect("localhost connect");
        stream_close(&mut heap, client).ok();
        let _ = accept.join();
    }

    #[test]
    fn from_abi_name_matches_tls_h_tags() {
        assert_eq!(
            IoErrorTag::from_abi_name("WouldBlock"),
            IoErrorTag::WouldBlock
        );
        assert_eq!(
            IoErrorTag::from_abi_name("Certificate"),
            IoErrorTag::Certificate
        );
        assert_eq!(
            IoErrorTag::from_abi_name("Handshake"),
            IoErrorTag::Handshake
        );
        assert_eq!(IoErrorTag::from_abi_name("TimedOut"), IoErrorTag::TimedOut);
        assert_eq!(IoErrorTag::from_abi_name("not-a-tag"), IoErrorTag::Other);
        assert_eq!(
            IoErrorTag::from_abi_name("WouldBlock"),
            IoErrorTag::from_abi(0)
        );
        assert_eq!(
            IoErrorTag::from_abi_name("Handshake"),
            IoErrorTag::from_abi(11)
        );
    }

    #[test]
    fn alloc_io_error_encodes_new_tags() {
        let mut heap = Heap::default();
        for (tag, expected) in [
            (IoErrorTag::TimedOut, 8u32),
            (IoErrorTag::Truncated, 9),
            (IoErrorTag::Certificate, 10),
            (IoErrorTag::Handshake, 11),
        ] {
            let v = alloc_io_error(&mut heap, tag);
            assert_eq!(enum_tag(&heap, v), Some(expected));
        }
        assert_eq!(BUILTIN_IO_ERROR_VARIANTS.len(), 12);
        assert_eq!(BUILTIN_IO_ERROR_VARIANTS[8], "TimedOut");
        assert_eq!(BUILTIN_IO_ERROR_VARIANTS[11], "Handshake");
    }

    fn tcp_connected_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let writer = TcpStream::connect(addr).expect("connect");
        let (reader, _) = listener.accept().expect("accept");
        (reader, writer)
    }

    fn tcp_stream_pair(heap: &mut Heap) -> (Value, TcpStream) {
        let (reader, writer) = tcp_connected_pair();
        let stream =
            alloc_stream(heap, NativeHandle::Tcp(reader), StreamKind::Tcp).expect("alloc read end");
        (stream, writer)
    }

    #[test]
    fn await_readable_returns_ok_when_already_ready() {
        let mut heap = Heap::default();
        let (stream, mut write) = tcp_stream_pair(&mut heap);
        write.write_all(b"a").expect("write");
        let _ = take_pending_io_park();
        let v = stream_await_readable(&mut heap, stream)
            .expect("await")
            .expect("already ready should not park");
        assert_eq!(enum_tag(&heap, v), Some(0));
        assert!(take_pending_io_park().is_none());
        stream_close(&mut heap, stream).unwrap();
        drop(write);
    }

    #[test]
    fn await_readable_parks_when_not_ready() {
        let mut heap = Heap::default();
        let (stream, write) = tcp_stream_pair(&mut heap);
        stream_set_read_timeout(&mut heap, stream, 123).unwrap();
        let _ = take_pending_io_park();
        let parked = stream_await_readable(&mut heap, stream).expect("await");
        assert!(parked.is_none(), "empty socket must park the VM");
        let req = take_pending_io_park().expect("park request");
        assert_eq!(req.interest, Interest::Readable);
        assert_eq!(req.timeout, Some(Duration::from_millis(123)));
        assert_eq!(req.handle, stream_wait_handle(&mut heap, stream).unwrap());
        stream_close(&mut heap, stream).unwrap();
        drop(write);
    }

    #[test]
    fn await_writable_returns_ok_for_empty_pipe_write_end() {
        let mut heap = Heap::default();
        let (reader, writer) = tcp_connected_pair();
        let stream = alloc_stream(&mut heap, NativeHandle::Tcp(writer), StreamKind::Tcp)
            .expect("alloc write end");
        let _ = take_pending_io_park();
        let v = stream_await_writable(&mut heap, stream)
            .expect("await")
            .expect("connected TCP write end is writable");
        assert_eq!(enum_tag(&heap, v), Some(0));
        assert!(take_pending_io_park().is_none());
        stream_close(&mut heap, stream).unwrap();
        drop(reader);
    }

    #[test]
    fn await_readable_on_closed_stream_errors() {
        let mut heap = Heap::default();
        let (stream, write) = tcp_stream_pair(&mut heap);
        stream_close(&mut heap, stream).unwrap();
        let err = stream_await_readable(&mut heap, stream).unwrap_err();
        assert_eq!(err, IoErrorTag::AlreadyClosed);
        drop(write);
    }

    #[test]
    fn io_drive_without_host_state_returns_zero() {
        let mut heap = Heap::default();
        assert_eq!(io_drive(&mut heap).as_int(), 0);
    }

    #[test]
    fn io_drive_with_host_state_counts_ready_waiters() {
        let mut vm = crate::Machine::<64>::default();
        let io = std::sync::Arc::clone(vm.io_reactor());
        let (r, mut w) = tcp_connected_pair();
        let tok = io.register_wait(WaitHandle::from_tcp(&r), Interest::Readable);
        let _guard = crate::thread::HostStateGuard::enter(&mut vm);
        let mut heap = Heap::default();
        assert_eq!(io_drive(&mut heap).as_int(), 0);
        w.write_all(b"q").expect("write");
        assert_eq!(io_drive(&mut heap).as_int(), 1);
        io.cancel_wait(tok);
        drop(r);
        drop(w);
    }

    #[test]
    fn io_wait_ready_without_host_state_returns_zero() {
        let mut heap = Heap::default();
        assert_eq!(io_wait_ready(&mut heap).as_int(), 0);
    }

    #[test]
    fn io_wait_ready_blocks_until_waiter_ready() {
        let mut vm = crate::Machine::<64>::default();
        let io = std::sync::Arc::clone(vm.io_reactor());
        let (r, mut w) = tcp_connected_pair();
        let tok = io.register_wait(WaitHandle::from_tcp(&r), Interest::Readable);
        let _guard = crate::thread::HostStateGuard::enter(&mut vm);
        let mut heap = Heap::default();
        // Make readable before wait so we don't hang the unit test.
        w.write_all(b"q").expect("write");
        assert!(io_wait_ready(&mut heap).as_int() >= 1);
        io.cancel_wait(tok);
        drop(r);
        drop(w);
    }
}
