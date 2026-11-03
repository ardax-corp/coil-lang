//! Native code entry points (feature/native, step 1).
//!
//! A native backend (the `coil-native` crate) compiles some function
//! bodies to machine code and installs them here, keyed by the bytecode PC
//! of the function's entry. A `CALL` to such a PC runs the native code
//! instead of opening a VM frame.
//!
//! The first tier only compiles *pure* functions: their values are ints,
//! floats and bools, and they call only other native functions. Such a
//! function has no effect but its result, so whenever native code cannot
//! give the VM's exact answer (an int trap, a recursion deeper than the
//! native stack budget, anything it does not model) it *bails*: it sets
//! [`NativeCtx::bail`] and returns, and the VM runs the same call from the
//! start in bytecode. The VM then raises the same panic, at the same
//! source location, as it always did. See `docs/internals/native.md`.

/// Shared state between the VM and native code. `#[repr(C)]`: the native
/// code reads and writes these fields at fixed offsets.
#[repr(C)]
#[derive(Debug)]
pub struct NativeCtx {
    /// Non-zero once native code gave up on the current call.
    pub bail: u64,
    /// Native code bails when its stack pointer drops below this address.
    pub stack_limit: u64,
}

/// Byte offset of [`NativeCtx::bail`].
pub const NATIVE_CTX_BAIL: i32 = 0;
/// Byte offset of [`NativeCtx::stack_limit`].
pub const NATIVE_CTX_STACK_LIMIT: i32 = 8;

/// How much native stack a call from the VM may use before it bails to the
/// VM (which then runs the call with its own frame limits).
pub const NATIVE_STACK_BUDGET: u64 = 256 * 1024;

/// Entry of a native function: `args` holds `arity` VM words, the result
/// is one VM word. Sets `ctx.bail` instead of returning a result when it
/// gives up.
pub type NativeEntryFn = unsafe extern "C" fn(ctx: *mut NativeCtx, args: *const u64) -> u64;

/// One compiled function.
#[derive(Clone, Copy, Debug)]
pub struct NativeEntry {
    /// Bytecode PC of the function's entry (its `CALL` target).
    pub pc: u32,
    pub arity: u16,
    pub entry: NativeEntryFn,
}

/// The compiled functions of one program, indexed by entry PC. Shared
/// (`Arc`) by every machine that runs the program.
#[derive(Default)]
pub struct NativeCode {
    /// `by_pc[pc]` is `index + 1` into `entries`, or `0`.
    by_pc: Vec<u32>,
    entries: Vec<NativeEntry>,
}

impl NativeCode {
    /// `code_len` is the program's bytecode length; entries past it are
    /// dropped.
    pub fn new(code_len: usize, entries: Vec<NativeEntry>) -> Self {
        let mut by_pc = vec![0u32; code_len];
        let mut kept = Vec::with_capacity(entries.len());
        for e in entries {
            if let Some(slot) = by_pc.get_mut(e.pc as usize) {
                kept.push(e);
                *slot = kept.len() as u32;
            }
        }
        Self { by_pc, entries: kept }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    pub(crate) fn lookup(&self, pc: usize) -> Option<NativeEntry> {
        let i = *self.by_pc.get(pc)?;
        if i == 0 {
            return None;
        }
        Some(self.entries[i as usize - 1])
    }
}

/// One machine's view of the native code: the shared code plus its own
/// counters and bail state.
pub(crate) struct NativeTable {
    pub(crate) code: std::sync::Arc<NativeCode>,
    /// Calls that ran natively / bailed to the VM (`COIL_NATIVE_STATS=1`).
    pub(crate) hits: u64,
    pub(crate) bails: u64,
    /// Operand stack base of the last call that bailed: calls whose base
    /// is above it run in the VM (`usize::MAX`: none).
    pub(crate) quiet_above: usize,
}

impl NativeTable {
    pub(crate) fn new(code: std::sync::Arc<NativeCode>) -> Self {
        Self { code, hits: 0, bails: 0, quiet_above: usize::MAX }
    }

    #[inline]
    pub(crate) fn lookup(&self, pc: usize) -> Option<NativeEntry> {
        self.code.lookup(pc)
    }
}

/// Run `entry` with `args`. `None` when it bailed.
#[inline(never)]
pub(crate) fn call(entry: &NativeEntry, args: &[u64]) -> Option<u64> {
    let marker = 0u8;
    let sp = std::ptr::addr_of!(marker) as u64;
    let mut ctx = NativeCtx {
        bail: 0,
        stack_limit: sp.saturating_sub(NATIVE_STACK_BUDGET),
    };
    // SAFETY: the backend compiled `entry` for exactly `arity` argument
    // words, which `args` holds; `ctx` outlives the call.
    let out = unsafe { (entry.entry)(&mut ctx, args.as_ptr()) };
    (ctx.bail == 0).then_some(out)
}

static PENDING: std::sync::Mutex<Option<Vec<NativeEntry>>> = std::sync::Mutex::new(None);

/// Native code for the next [`crate::Machine::run_raw`] in this process
/// (the program's main machine); the machines it spawns share it.
pub fn install_for_next_run(entries: Vec<NativeEntry>) {
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(entries);
}

pub(crate) fn take_pending() -> Option<Vec<NativeEntry>> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).take()
}
