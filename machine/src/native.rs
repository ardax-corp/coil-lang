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
    /// Heap stores made so far, so a bail can take them back before the
    /// VM runs the call again (see [`Undo`]).
    pub undo: *mut Vec<Undo>,
}

/// One heap store native code made: the word it overwrote. A bail restores
/// them newest first, so the VM's re-run starts from the heap as the call
/// found it.
#[derive(Clone, Copy, Debug)]
pub enum Undo {
    Index { array: u64, index: i64, old: u64 },
    Field { object: u64, slot: usize, old: u64 },
}

/// Byte offset of [`NativeCtx::bail`].
pub const NATIVE_CTX_BAIL: i32 = 0;
/// Byte offset of [`NativeCtx::stack_limit`].
pub const NATIVE_CTX_STACK_LIMIT: i32 = 8;
/// Byte offset of [`NativeCtx::undo`].
pub const NATIVE_CTX_UNDO: i32 = 16;

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
    let mut undo = Vec::new();
    let mut ctx = NativeCtx {
        bail: 0,
        stack_limit: sp.saturating_sub(NATIVE_STACK_BUDGET),
        undo: &mut undo,
    };
    // SAFETY: the backend compiled `entry` for exactly `arity` argument
    // words, which `args` holds; `ctx` outlives the call.
    let out = unsafe { (entry.entry)(&mut ctx, args.as_ptr()) };
    if ctx.bail == 0 {
        return Some(out);
    }
    for u in undo.into_iter().rev() {
        match u {
            Undo::Index { array, index, old } => {
                if let Some(crate::memory::Object::Array(mut gc)) = object(array) {
                    gc.as_mut().store_indexed(index, common::Value::from(old), false);
                }
            }
            Undo::Field { object: o, slot, old } => {
                if let Some(crate::memory::Object::Instance(mut gc)) = object(o) {
                    gc.as_mut().set_slot(slot, common::Value::from(old));
                }
            }
        }
    }
    None
}

thread_local! {
    /// Per thread: a harness compiles and runs several programs at once.
    static PENDING: std::cell::RefCell<Option<Pending>> = const { std::cell::RefCell::new(None) };
}

/// Native code for the next [`crate::Machine::run_raw`] on this thread
/// (the program's main machine); the machines it spawns share it.
pub fn install_for_next_run(entries: Vec<NativeEntry>) {
    PENDING.with(|p| *p.borrow_mut() = Some(Pending::Entries(entries)));
}

/// [`install_for_next_run`] with code already built for the program.
pub fn install_shared_for_next_run(code: std::sync::Arc<NativeCode>) {
    PENDING.with(|p| *p.borrow_mut() = Some(Pending::Shared(code)));
}

pub(crate) enum Pending {
    Entries(Vec<NativeEntry>),
    Shared(std::sync::Arc<NativeCode>),
}

impl Pending {
    pub(crate) fn into_code(self, code_len: usize) -> std::sync::Arc<NativeCode> {
        match self {
            Self::Entries(entries) => std::sync::Arc::new(NativeCode::new(code_len, entries)),
            Self::Shared(code) => code,
        }
    }
}

pub(crate) fn take_pending() -> Option<Pending> {
    PENDING.with(|p| p.borrow_mut().take())
}

// ---------------------------------------------------------------------------
// Heap reads for native code (heap tier).
//
// Native code calls these with heap words it was handed. It never
// allocates and never re-enters the VM, so no collection runs while it
// holds them and every address stays a live object. Each helper does what
// the matching VM instruction does; where the VM would panic, it sets the
// bail flag and returns 0.

/// The object at a heap word, as the VM's `trusted_object` reads it.
fn object(addr: u64) -> Option<crate::memory::Object> {
    if addr == 0 {
        return None;
    }
    // SAFETY: see above; a non-zero heap word names a live object.
    unsafe { crate::memory::Object::from_header(addr) }
}

/// `Index` / `IndexUnchecked` (both checked here: bounds are cheap next to
/// the call). Out of bounds or not an array or tuple: bail.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `addr` is a heap word.
pub unsafe extern "C" fn coil_native_index(ctx: *mut NativeCtx, addr: u64, index: i64) -> u64 {
    let read = match object(addr) {
        Some(crate::memory::Object::Array(gc)) => gc.as_ref().read_indexed(index, false),
        Some(crate::memory::Object::Tuple(gc)) => {
            let elements = gc.as_ref().elements();
            usize::try_from(index).ok().and_then(|i| elements.get(i).copied())
        }
        _ => None,
    };
    match read {
        Some(v) => v.bits(),
        None => {
            // SAFETY: the caller passes its live context.
            unsafe { (*ctx).bail = 1 };
            0
        }
    }
}

/// `ArrayLen`.
pub extern "C" fn coil_native_len(addr: u64) -> u64 {
    (match object(addr) {
        Some(crate::memory::Object::Array(gc)) => gc.as_ref().len(),
        Some(crate::memory::Object::Tuple(gc)) => gc.as_ref().elements().len(),
        Some(crate::memory::Object::String(gc)) => gc.as_ref().data.len(),
        Some(crate::memory::Object::Instance(gc)) => {
            gc.as_ref().slot_len().unwrap_or_else(|| gc.as_ref().iter_fields().count())
        }
        _ => 0,
    }) as u64
}

/// `LoadField` of slot `index`.
pub extern "C" fn coil_native_field(addr: u64, index: u64) -> u64 {
    let index = index as usize;
    match object(addr) {
        Some(crate::memory::Object::Enum(e)) => e.as_ref().payload.get(index).map_or(0, |v| v.bits()),
        Some(crate::memory::Object::Instance(gc)) => {
            gc.as_ref().slot_len().and_then(|_| gc.as_ref().slot(index)).map_or(0, |v| v.bits())
        }
        _ => 0,
    }
}

/// Log a store into `ctx`'s undo list.
///
/// # Safety
/// `ctx` is a live [`NativeCtx`] whose `undo` points at a live list.
unsafe fn log(ctx: *mut NativeCtx, u: Undo) {
    unsafe { (*(*ctx).undo).push(u) };
}

/// Set the bail flag; returns `0` for the helper's result.
///
/// # Safety
/// `ctx` is a live [`NativeCtx`].
unsafe fn bail(ctx: *mut NativeCtx) -> u64 {
    unsafe { (*ctx).bail = 1 };
    0
}

/// `StoreIndex` / `StoreIndexUnchecked`: store `value` at `index` and
/// return it. Out of bounds or not an array: bail.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `addr` is a heap word.
pub unsafe extern "C" fn coil_native_store_index(ctx: *mut NativeCtx, addr: u64, index: i64, value: u64) -> u64 {
    let Some(crate::memory::Object::Array(mut gc)) = object(addr) else {
        return unsafe { bail(ctx) };
    };
    let Some(old) = gc.as_ref().read_indexed(index, false) else {
        return unsafe { bail(ctx) };
    };
    unsafe { log(ctx, Undo::Index { array: addr, index, old: old.bits() }) };
    gc.as_mut().store_indexed(index, common::Value::from(value), false);
    value
}

/// `SetField` of slot `index` on an instance with slots; return `value`.
/// Anything else: bail.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `addr` is a heap word.
pub unsafe extern "C" fn coil_native_set_field(ctx: *mut NativeCtx, addr: u64, index: u64, value: u64) -> u64 {
    let slot = index as usize;
    let Some(crate::memory::Object::Instance(mut gc)) = object(addr) else {
        return unsafe { bail(ctx) };
    };
    let Some(old) = gc.as_ref().slot_len().filter(|&n| slot < n).and_then(|_| gc.as_ref().slot(slot)) else {
        return unsafe { bail(ctx) };
    };
    unsafe { log(ctx, Undo::Field { object: addr, slot, old: old.bits() }) };
    gc.as_mut().set_slot(slot, common::Value::from(value));
    value
}
