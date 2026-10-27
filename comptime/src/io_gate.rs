//! IO natives on the compile-time VM, checked against the grants of the
//! evaluation running ([`set_active`]): `--comptime-read` / `-write` /
//! `-env` / `-net` for the module of the const, `comptime` expression or
//! macro. With no grants active (silent folding, `static let`) every one
//! of them fails the evaluation. While recording ([`start_recording`]),
//! each granted call is noted for the result cache.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::Value;
use compiler::macros::{ComptimeAccess, ComptimeIo};
use machine::{FfiError, Heap, HostClosureFn, NativeFn, Object};

thread_local! {
    static ACTIVE: RefCell<Option<Arc<ComptimeIo>>> = const { RefCell::new(None) };
    static RECORD: RefCell<Option<Vec<ComptimeAccess>>> = const { RefCell::new(None) };
    static DEADLINE: Cell<Option<(Instant, u64)>> = const { Cell::new(None) };
}

/// Start the clock of one evaluation: its IO calls fail once `secs` pass.
pub fn start_clock(secs: u64) {
    DEADLINE.with(|d| d.set(Some((Instant::now() + Duration::from_secs(secs), secs))));
}

/// Milliseconds one network wait may take now: at most
/// [`compiler::ComptimeLimits::NET_OP_SECS`], and a little past the end of
/// the clock, so a wait cut short by it ends the evaluation late
/// ([`clock_ran_out`]).
fn net_wait_ms() -> i64 {
    let op = Duration::from_secs(compiler::ComptimeLimits::NET_OP_SECS);
    let left = DEADLINE
        .with(|d| d.get())
        .map_or(op, |(end, _)| end.saturating_duration_since(Instant::now()) + Duration::from_millis(100));
    op.min(left).as_millis().max(1) as i64
}

/// The error of an evaluation that is past its clock.
pub fn clock_ran_out() -> Option<String> {
    match DEADLINE.with(|d| d.get()) {
        Some((end, secs)) if Instant::now() >= end => Some(format!(
            "it ran out of time: an evaluation may take {secs} s (`--comptime-timeout SECS` raises it)"
        )),
        _ => None,
    }
}

fn check_clock() -> Result<(), String> {
    clock_ran_out().map_or(Ok(()), Err)
}

/// Note the IO this thread's compile-time VM does from now on.
pub fn start_recording() {
    RECORD.with(|r| *r.borrow_mut() = Some(Vec::new()));
}

/// The IO noted since [`start_recording`], each once; stops recording.
pub fn take_recording() -> Vec<ComptimeAccess> {
    RECORD.with(|r| r.borrow_mut().take().unwrap_or_default())
}

fn recording() -> bool {
    RECORD.with(|r| r.borrow().is_some())
}

fn note(access: ComptimeAccess) {
    RECORD.with(|r| {
        if let Some(list) = r.borrow_mut().as_mut()
            && !list.contains(&access)
        {
            list.push(access);
        }
    });
}

/// A read of `path`, with what it sees before the native runs.
fn note_read(path: &str) {
    if !recording() {
        return;
    }
    let path = compiler::macros::absolute_path(std::path::Path::new(path));
    let seen = RECORD.with(|r| {
        r.borrow()
            .iter()
            .flatten()
            .any(|a| matches!(a, ComptimeAccess::Read { path: p, .. } if *p == path))
    });
    if !seen {
        let digest = compiler::comptime_cache::path_digest(&path);
        note(ComptimeAccess::Read { path, digest });
    }
}

fn note_write(path: &str) {
    if recording() {
        note(ComptimeAccess::Write(compiler::macros::absolute_path(std::path::Path::new(path))));
    }
}

/// Make `io` the grants of what this thread's compile-time VM runs next;
/// returns the previous ones.
pub fn set_active(io: Option<Arc<ComptimeIo>>) -> Option<Arc<ComptimeIo>> {
    ACTIVE.with(|a| a.replace(io))
}

/// What a gated native checks before it runs.
#[derive(Clone, Copy)]
enum Gate {
    /// Argument `0` is a path it reads.
    Read,
    /// Arguments `.0..` (all of them up to `.1`) are paths it writes.
    Write(usize, usize),
    /// `fs_copy(from, to)`.
    Copy,
    /// `open(path, mode)`: as its mode says.
    Open,
    /// Argument `0` names an environment variable.
    Env,
    /// A stream operation: the stream came from a granted `open` or connect.
    Stream,
    /// `tcp_connect(host, port)` / `tcp_connect_timeout(host, port, ms)`.
    Net,
}

fn gate_of(name: &str) -> Option<Gate> {
    Some(match name {
        "fs_exists" | "fs_is_file" | "fs_is_dir" | "fs_is_symlink" | "fs_metadata" | "fs_read_link" | "fs_list_dir"
        | "fs_realpath" => Gate::Read,
        "fs_create_dir" | "fs_create_dir_all" | "fs_remove_file" | "fs_remove_dir" | "fs_remove_dir_all" => {
            Gate::Write(0, 1)
        }
        "fs_rename" => Gate::Write(0, 2),
        // The link is what it creates; reading through it is checked again.
        "fs_symlink" => Gate::Write(1, 2),
        "fs_copy" => Gate::Copy,
        "open" => Gate::Open,
        "env_var" => Gate::Env,
        "read" | "write" | "close" | "write_from" | "await_readable" | "await_writable" | "tcp_peer_addr"
        | "tcp_local_addr" | "tcp_set_nodelay" | "tcp_shutdown" => Gate::Stream,
        "tcp_connect" | "tcp_connect_timeout" => Gate::Net,
        _ => return None,
    })
}

fn text(heap: &Heap, v: Value) -> Option<String> {
    match heap.find_object_by_addr(v.bits()) {
        Some(Object::String(s)) => Some(s.as_ref().data.to_string()),
        _ => None,
    }
}

fn check(io: &ComptimeIo, gate: Gate, heap: &Heap, args: &[Value]) -> Result<(), String> {
    let arg = |i: usize| args.get(i).and_then(|&v| text(heap, v)).ok_or_else(|| "a path argument is not a string".to_string());
    let read = |path: &str| io.check_read(path).map(|()| note_read(path));
    let write = |path: &str| io.check_write(path).map(|()| note_write(path));
    match gate {
        Gate::Read => read(&arg(0)?),
        Gate::Write(from, to) => (from..to).try_for_each(|i| write(&arg(i)?)),
        Gate::Copy => {
            read(&arg(0)?)?;
            write(&arg(1)?)
        }
        Gate::Open => {
            let path = arg(0)?;
            let mode = args.get(1).and_then(|&v| text(heap, v));
            let caps = common::open_mode_caps(mode.as_deref());
            if caps.contains(common::Caps::READ) {
                read(&path)?;
            }
            if caps.contains(common::Caps::WRITE) {
                write(&path)?;
            }
            Ok(())
        }
        Gate::Env => {
            let name = arg(0)?;
            io.check_env(&name)?;
            note(ComptimeAccess::Env(name));
            Ok(())
        }
        Gate::Stream => Ok(()),
        Gate::Net => {
            let host = arg(0)?;
            let port = args.get(1).map_or(0, |v| v.as_int());
            io.check_net(&host, port)?;
            note(ComptimeAccess::Net(format!("{host}:{port}")));
            Ok(())
        }
    }
}

/// `native` checked against the active grants, or `None` when `name` is
/// not an IO native the grants cover.
pub fn gated(name: &str, native: Arc<dyn NativeFn>) -> Option<Arc<dyn NativeFn>> {
    let gate = gate_of(name)?;
    let name = name.to_string();
    let sig = native.signature().clone();
    Some(Arc::new(HostClosureFn::new(sig, move |heap, args| {
        let io = ACTIVE.with(|a| a.borrow().clone());
        let Some(io) = io else {
            return Err(FfiError::Panic(format!(
                "`{name}` is not available here: only a `const`, a `comptime` expression or a macro may do IO while compiling"
            )));
        };
        check_clock().map_err(FfiError::Panic)?;
        check(&io, gate, heap, args).map_err(FfiError::Panic)?;
        if matches!(gate, Gate::Net) {
            return Ok(Some(connect(heap, args)));
        }
        let out = native.invoke(heap, args);
        // A wait that ran into the end of the clock ends the evaluation.
        check_clock().map_err(FfiError::Panic)?;
        out
    })))
}

/// A granted connect, waiting at most [`net_wait_ms`] (less when the call
/// asked for less), with each read and write on the stream bounded by what
/// [`net_wait_ms`] is then.
fn connect(heap: &mut Heap, args: &[Value]) -> Value {
    use machine::io::{as_result_value, stream_set_read_timeout, stream_set_write_timeout, tcp_connect_timeout, value_as_string};
    let host = value_as_string(heap, args[0]).unwrap_or_default();
    let port = args[1].as_int();
    let wait = net_wait_ms();
    let ms = match args.get(2).map(|v| v.as_int()) {
        Some(asked) if asked > 0 => asked.min(wait),
        _ => wait,
    };
    let stream = tcp_connect_timeout(heap, &host, port, ms);
    if let Ok(s) = stream {
        let wait = net_wait_ms();
        let _ = stream_set_read_timeout(heap, s, wait);
        let _ = stream_set_write_timeout(heap, s, wait);
    }
    as_result_value(heap, stream)
}
