//! IO natives on the compile-time VM, checked against the grants of the
//! evaluation running ([`set_active`]): `--comptime-read` / `-write` /
//! `-env` / `-exec`. With no grants active (macros, silent folding) every
//! one of them fails the evaluation.

use std::cell::RefCell;
use std::sync::Arc;

use common::Value;
use compiler::macros::ComptimeIo;
use machine::{FfiError, Heap, HostClosureFn, NativeFn, Object};

thread_local! {
    static ACTIVE: RefCell<Option<Arc<ComptimeIo>>> = const { RefCell::new(None) };
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
    /// Argument `0` is a program to run.
    Exec,
    /// A stream operation: the stream came from a granted `open`.
    Stream,
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
        "env_exec" => Gate::Exec,
        "read" | "write" | "close" | "write_from" => Gate::Stream,
        _ => return None,
    })
}

fn text(heap: &Heap, v: Value) -> Option<String> {
    match heap.find_object_by_addr(v.raw() as u64) {
        Some(Object::String(s)) => Some(s.as_ref().data.to_string()),
        _ => None,
    }
}

fn check(io: &ComptimeIo, gate: Gate, heap: &Heap, args: &[Value]) -> Result<(), String> {
    let arg = |i: usize| args.get(i).and_then(|&v| text(heap, v)).ok_or_else(|| "a path argument is not a string".to_string());
    match gate {
        Gate::Read => io.check_read(&arg(0)?),
        Gate::Write(from, to) => (from..to).try_for_each(|i| io.check_write(&arg(i)?)),
        Gate::Copy => {
            io.check_read(&arg(0)?)?;
            io.check_write(&arg(1)?)
        }
        Gate::Open => {
            let path = arg(0)?;
            let mode = args.get(1).and_then(|&v| text(heap, v));
            let caps = common::open_mode_caps(mode.as_deref());
            if caps.contains(common::Caps::READ) {
                io.check_read(&path)?;
            }
            if caps.contains(common::Caps::WRITE) {
                io.check_write(&path)?;
            }
            Ok(())
        }
        Gate::Env => io.check_env(&arg(0)?),
        Gate::Exec => io.check_exec(&arg(0)?),
        Gate::Stream => Ok(()),
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
                "`{name}` is not available here: only a `const` or `comptime` expression may do IO while compiling"
            )));
        };
        check(&io, gate, heap, args).map_err(FfiError::Panic)?;
        native.invoke(heap, args)
    })))
}
