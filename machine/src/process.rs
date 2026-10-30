//! Child-process host natives behind the embedded `process` module: spawn a
//! program (no shell), take its pipes as `Stream`s, wait, kill, signal.
//!
//! Built on `std::process`. A spawned child lives in a process-wide table
//! keyed by a positive id; the `process::Child` class holds that id and
//! releases it from its finalizer. Pipe ends become ordinary non-blocking
//! file streams, so `io::read` / `write` / `close`, `wait_readable` and the
//! task scheduler's IO waits work on them unchanged.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use common::Value;

use crate::io::{
    IoErrorTag, alloc_io_error, alloc_option_none, alloc_option_some, alloc_result_err,
    alloc_result_ok, alloc_stream, as_result_unit, as_result_value,
};
use crate::io_handle::NativeHandle;
use crate::memory::{Heap, ObjTuple, Object, StreamKind};

/// `process::Stdio` tags, as the embedded module packs them into the flags
/// word: two bits per stream, stdin in bits 0–1, stdout in 2–3, stderr in
/// 4–5. Bit 6 ([`FLAG_ENV_CLEAR`]) starts from an empty environment.
const STDIO_INHERIT: i64 = 0;
const STDIO_PIPE: i64 = 1;
const STDIO_NULL: i64 = 2;
const FLAG_ENV_CLEAR: i64 = 1 << 6;

/// Which pipe `process_take_pipe` takes.
const PIPE_STDIN: i64 = 0;
const PIPE_STDOUT: i64 = 1;
const PIPE_STDERR: i64 = 2;

type Entry = Arc<Mutex<Child>>;

fn children() -> &'static Mutex<HashMap<i64, Entry>> {
    static CHILDREN: OnceLock<Mutex<HashMap<i64, Entry>>> = OnceLock::new();
    CHILDREN.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT_ID: AtomicI64 = AtomicI64::new(1);

fn entry(id: i64) -> Result<Entry, IoErrorTag> {
    let table = children().lock().unwrap_or_else(|e| e.into_inner());
    table.get(&id).cloned().ok_or(IoErrorTag::AlreadyClosed)
}

fn with_child<T>(id: i64, f: impl FnOnce(&mut Child) -> Result<T, IoErrorTag>) -> Result<T, IoErrorTag> {
    let entry = entry(id)?;
    let mut child = entry.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut child)
}

fn io_tag(e: std::io::Error) -> IoErrorTag {
    IoErrorTag::from_kind(e.kind())
}

fn heap_string(heap: &Heap, v: Value) -> Result<String, IoErrorTag> {
    match heap.find_object_by_addr(v.bits()) {
        Some(Object::String(gc)) => Ok(gc.as_ref().data.to_string()),
        _ => Err(IoErrorTag::InvalidInput),
    }
}

fn heap_strings(heap: &Heap, v: Value) -> Result<Vec<String>, IoErrorTag> {
    match heap.find_object_by_addr(v.bits()) {
        Some(Object::Array(gc)) => gc.as_ref().iter().map(|e| heap_string(heap, e)).collect(),
        _ => Err(IoErrorTag::InvalidInput),
    }
}

fn stdio(mode: i64) -> Result<Stdio, IoErrorTag> {
    match mode {
        STDIO_INHERIT => Ok(Stdio::inherit()),
        STDIO_PIPE => Ok(Stdio::piped()),
        STDIO_NULL => Ok(Stdio::null()),
        _ => Err(IoErrorTag::InvalidInput),
    }
}

/// The exit code, or `128 + signal` for a child a POSIX signal ended.
fn exit_code(status: ExitStatus) -> i64 {
    if let Some(code) = status.code() {
        return code as i64;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig as i64;
        }
    }
    -1
}

/// What `process_spawn` starts: a program, its arguments, environment
/// changes and working directory.
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    /// `key, value` pairs set in the child; a key with no value later
    /// (an odd tail) is ignored.
    pub env: Vec<String>,
    /// Keys removed from the child's environment.
    pub env_remove: Vec<String>,
    /// Empty: inherit the current directory.
    pub cwd: String,
    /// [`STDIO_INHERIT`] … per stream and [`FLAG_ENV_CLEAR`], packed as the
    /// module packs them.
    pub flags: i64,
}

/// Start `spec`; the new child's table id.
pub fn spawn(spec: &SpawnSpec) -> Result<i64, IoErrorTag> {
    let has_nul = |s: &String| s.contains('\0');
    if has_nul(&spec.program)
        || spec.args.iter().any(has_nul)
        || spec.env.iter().any(has_nul)
        || spec.env_remove.iter().any(has_nul)
        || has_nul(&spec.cwd)
    {
        return Err(IoErrorTag::InvalidInput);
    }
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args);
    if spec.flags & FLAG_ENV_CLEAR != 0 {
        cmd.env_clear();
    }
    for key in &spec.env_remove {
        cmd.env_remove(key);
    }
    for [key, value] in spec.env.as_chunks::<2>().0 {
        cmd.env(key, value);
    }
    if !spec.cwd.is_empty() {
        cmd.current_dir(&spec.cwd);
    }
    cmd.stdin(stdio(spec.flags & 3)?);
    cmd.stdout(stdio((spec.flags >> 2) & 3)?);
    cmd.stderr(stdio((spec.flags >> 4) & 3)?);
    let child = cmd.spawn().map_err(io_tag)?;
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    children()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, Arc::new(Mutex::new(child)));
    Ok(id)
}

/// Take the child's `which` pipe as an owned handle; `None` when it was
/// not piped or is already taken.
pub fn take_pipe(id: i64, which: i64) -> Result<Option<NativeHandle>, IoErrorTag> {
    with_child(id, |child| {
        let handle = match which {
            PIPE_STDIN => child.stdin.take().map(pipe_handle),
            PIPE_STDOUT => child.stdout.take().map(pipe_handle),
            PIPE_STDERR => child.stderr.take().map(pipe_handle),
            _ => return Err(IoErrorTag::InvalidInput),
        };
        handle.transpose()
    })
}

#[cfg(unix)]
fn pipe_handle(pipe: impl Into<std::os::fd::OwnedFd>) -> Result<NativeHandle, IoErrorTag> {
    Ok(NativeHandle::File(std::fs::File::from(pipe.into())))
}

#[cfg(windows)]
fn pipe_handle(pipe: impl Into<std::os::windows::io::OwnedHandle>) -> Result<NativeHandle, IoErrorTag> {
    Ok(NativeHandle::File(std::fs::File::from(pipe.into())))
}

#[cfg(not(any(unix, windows)))]
fn pipe_handle<P>(_pipe: P) -> Result<NativeHandle, IoErrorTag> {
    Err(IoErrorTag::Other)
}

/// Wait for the child to end (closing a stdin pipe still held here first).
pub fn wait(id: i64) -> Result<i64, IoErrorTag> {
    with_child(id, |child| {
        child.stdin = None;
        child.wait().map(exit_code).map_err(io_tag)
    })
}

/// The exit code if the child has ended, without waiting.
pub fn try_wait(id: i64) -> Result<Option<i64>, IoErrorTag> {
    with_child(id, |child| child.try_wait().map(|s| s.map(exit_code)).map_err(io_tag))
}

/// Read the stdout / stderr pipes still held here to their end and wait:
/// `(exit code, stdout, stderr)`. A pipe already taken reads as empty.
pub fn output(id: i64) -> Result<(i64, Vec<u8>, Vec<u8>), IoErrorTag> {
    with_child(id, |child| {
        child.stdin = None;
        let out = child.stdout.take();
        let err = child.stderr.take();
        // Drain stderr on a helper thread so neither pipe can fill up and
        // stall the child while the other is read.
        let err_reader = err.map(|mut e| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                e.read_to_end(&mut buf).map(|_| buf)
            })
        });
        let mut out_buf = Vec::new();
        if let Some(mut o) = out {
            o.read_to_end(&mut out_buf).map_err(io_tag)?;
        }
        let err_buf = match err_reader {
            Some(t) => t.join().map_err(|_| IoErrorTag::Other)?.map_err(io_tag)?,
            None => Vec::new(),
        };
        let code = child.wait().map(exit_code).map_err(io_tag)?;
        Ok((code, out_buf, err_buf))
    })
}

/// Force the child to end (`SIGKILL` / `TerminateProcess`). A child that
/// already ended is not an error.
pub fn kill(id: i64) -> Result<(), IoErrorTag> {
    with_child(id, |child| match child.kill() {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
        Err(e) => Err(io_tag(e)),
    })
}

/// Send POSIX signal `sig` to the child. `InvalidInput` on Windows (use
/// `kill`) and for a child that was already waited for.
pub fn signal(id: i64, sig: i64) -> Result<(), IoErrorTag> {
    with_child(id, |child| {
        #[cfg(unix)]
        {
            if child.try_wait().map_err(io_tag)?.is_some() {
                return Err(IoErrorTag::InvalidInput);
            }
            let sig = libc::c_int::try_from(sig).map_err(|_| IoErrorTag::InvalidInput)?;
            // SAFETY: `kill` takes a pid and a signal number; the child is
            // not reaped (checked above), so its pid is still ours.
            if unsafe { libc::kill(child.id() as libc::pid_t, sig) } != 0 {
                return Err(io_tag(std::io::Error::last_os_error()));
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = (child, sig);
            Err(IoErrorTag::InvalidInput)
        }
    })
}

/// The OS process id.
pub fn pid(id: i64) -> Result<i64, IoErrorTag> {
    with_child(id, |child| Ok(child.id() as i64))
}

/// Forget the child (its class's finalizer). A child still running keeps
/// running; its pipes still held here close.
pub fn release(id: i64) {
    children().lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
}

fn host_spawn(heap: &mut Heap, args: &[Value]) -> Value {
    let r = (|| {
        let spec = SpawnSpec {
            program: heap_string(heap, args[0])?,
            args: heap_strings(heap, args[1])?,
            env: heap_strings(heap, args[2])?,
            env_remove: heap_strings(heap, args[3])?,
            cwd: heap_string(heap, args[4])?,
            flags: args[5].as_int(),
        };
        spawn(&spec).map(Value::from)
    })();
    as_result_value(heap, r)
}

fn host_take_pipe(heap: &mut Heap, args: &[Value]) -> Value {
    let stream = match take_pipe(args[0].as_int(), args[1].as_int()) {
        Ok(Some(handle)) => alloc_stream(heap, handle, StreamKind::File).ok(),
        _ => None,
    };
    crate::host_enum::pack_option_edge(heap, stream).unwrap_or_else(|e| panic!("{e}"))
}

fn host_wait(heap: &mut Heap, args: &[Value]) -> Value {
    let r = wait(args[0].as_int()).map(Value::from);
    as_result_value(heap, r)
}

fn host_try_wait(heap: &mut Heap, args: &[Value]) -> Value {
    match try_wait(args[0].as_int()) {
        Ok(None) => {
            let none = alloc_option_none(heap);
            alloc_result_ok(heap, none)
        }
        Ok(Some(code)) => {
            let some = alloc_option_some(heap, Value::from(code));
            alloc_result_ok(heap, some)
        }
        Err(tag) => {
            let err = alloc_io_error(heap, tag);
            alloc_result_err(heap, err)
        }
    }
}

fn host_output(heap: &mut Heap, args: &[Value]) -> Value {
    let r = output(args[0].as_int()).map(|(code, out, err)| {
        let out = heap.intern(String::from_utf8_lossy(&out).into_owned());
        let err = heap.intern(String::from_utf8_lossy(&err).into_owned());
        let (obj, _) = heap.alloc(
            ObjTuple::new(vec![
                Value::from(code),
                Value::from(out.as_ptr() as *mut u8 as u64),
                Value::from(err.as_ptr() as *mut u8 as u64),
            ]),
            Object::Tuple,
        );
        Value::from(obj.addr())
    });
    as_result_value(heap, r)
}

fn host_kill(heap: &mut Heap, args: &[Value]) -> Value {
    let r = kill(args[0].as_int());
    as_result_unit(heap, r)
}

fn host_signal(heap: &mut Heap, args: &[Value]) -> Value {
    let r = signal(args[0].as_int(), args[1].as_int());
    as_result_unit(heap, r)
}

fn host_pid(heap: &mut Heap, args: &[Value]) -> Value {
    let r = pid(args[0].as_int()).map(Value::from);
    as_result_value(heap, r)
}

fn host_release(_heap: &mut Heap, args: &[Value]) -> Value {
    release(args[0].as_int());
    Value::from(0_i64)
}

/// Pipeline wiring: `(registry_name, arity, host_fn)`, in HostInvoke order.
pub const PROCESS_WIRING: &[(&str, usize, crate::HostValueFn)] = &[
    (common::PROCESS_SPAWN_NATIVE, 6, host_spawn),
    (common::PROCESS_TAKE_PIPE_NATIVE, 2, host_take_pipe),
    (common::PROCESS_WAIT_NATIVE, 1, host_wait),
    (common::PROCESS_TRY_WAIT_NATIVE, 1, host_try_wait),
    (common::PROCESS_OUTPUT_NATIVE, 1, host_output),
    (common::PROCESS_KILL_NATIVE, 1, host_kill),
    (common::PROCESS_SIGNAL_NATIVE, 2, host_signal),
    (common::PROCESS_PID_NATIVE, 1, host_pid),
    (common::PROCESS_RELEASE_NATIVE, 1, host_release),
];

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn spec(program: &str, args: &[&str], flags: i64) -> SpawnSpec {
        SpawnSpec {
            program: program.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: Vec::new(),
            env_remove: Vec::new(),
            cwd: String::new(),
            flags,
        }
    }

    const PIPED: i64 = STDIO_PIPE | (STDIO_PIPE << 2) | (STDIO_PIPE << 4);

    #[test]
    fn output_collects_both_pipes_and_the_code() {
        let id = spawn(&spec("sh", &["-c", "printf out; printf err >&2; exit 3"], PIPED)).unwrap();
        let (code, out, err) = output(id).unwrap();
        assert_eq!((code, out.as_slice(), err.as_slice()), (3, &b"out"[..], &b"err"[..]));
        release(id);
        assert_eq!(wait(id), Err(IoErrorTag::AlreadyClosed));
    }

    #[test]
    fn a_signal_shows_as_128_plus_its_number() {
        let id = spawn(&spec("sleep", &["5"], 0)).unwrap();
        signal(id, libc::SIGTERM as i64).unwrap();
        assert_eq!(wait(id), Ok(128 + libc::SIGTERM as i64));
        assert_eq!(signal(id, libc::SIGTERM as i64), Err(IoErrorTag::InvalidInput));
        release(id);
    }

    #[test]
    fn a_missing_program_is_not_found() {
        assert_eq!(spawn(&spec("/nonexistent/coil-no-such-program", &[], 0)), Err(IoErrorTag::NotFound));
    }

    #[test]
    fn env_and_cwd_reach_the_child() {
        let mut s = spec("sh", &["-c", "printf '%s:%s' \"$COIL_P\" \"$(pwd)\""], STDIO_PIPE << 2);
        s.env = vec!["COIL_P".into(), "v".into()];
        s.cwd = "/".into();
        let id = spawn(&s).unwrap();
        let (_, out, _) = output(id).unwrap();
        assert_eq!(out, b"v:/");
        release(id);
    }

    #[test]
    fn a_pipe_is_taken_once() {
        let id = spawn(&spec("true", &[], STDIO_PIPE << 2)).unwrap();
        assert!(matches!(take_pipe(id, PIPE_STDOUT), Ok(Some(_))));
        assert!(matches!(take_pipe(id, PIPE_STDOUT), Ok(None)));
        assert!(matches!(take_pipe(id, PIPE_STDIN), Ok(None)));
        assert_eq!(wait(id), Ok(0));
        release(id);
    }
}
