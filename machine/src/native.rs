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
    /// The undo log: heap stores made so far, so a bail can take them back
    /// before the VM runs the call again. `undo_len` entries of
    /// [`UndoEntry`] at `undo_buf`, room for `undo_cap`; native code
    /// appends inline and calls [`coil_native_undo_grow`] when it is full.
    pub undo_buf: *mut UndoEntry,
    pub undo_len: u64,
    pub undo_cap: u64,
    /// The storage behind `undo_buf`.
    pub undo_vec: *mut Vec<UndoEntry>,
    /// The machine's heap, for helpers that allocate (null: they bail).
    pub heap: *mut crate::memory::Heap,
    /// Objects helpers allocated during this call. No collection runs
    /// while native code holds raw heap words, so the VM collects after
    /// the call returns; past [`NATIVE_ALLOC_CAP`] with a collection due,
    /// helpers bail instead and the VM's re-run collects as it goes.
    pub allocs: u64,
    /// The second result word of a body that returns two
    /// ([`NativeEntry::ret_words`] `2`): the entry stores it here.
    pub ret_hi: u64,
    /// The program's string table and the machine's cache of its
    /// interned objects (`STRING`), `strings_len` entries each.
    pub strings: *const String,
    pub string_cache: *mut common::Value,
    pub strings_len: u64,
    /// Early frees (`FreeEarly`) the VM makes once the call returns:
    /// `(word, deep)`. A free cannot be undone, so none happens inside a
    /// call that may still bail.
    pub frees: *mut Vec<(u64, bool)>,
    /// Non-zero when the program has `drop` finalizers: an early free may
    /// then run one, so `FreeEarly` bails and the VM runs it.
    pub has_finalizers: u64,
    /// The machine's statics (`LoadStatic` / `StoreStatic`), `statics_len`
    /// words.
    pub statics: *mut common::Value,
    pub statics_len: u64,
    /// Non-zero once the call made an effect it cannot take back (a
    /// `print`, a host call). From then on it never bails: a trap panics
    /// natively, as the VM would at the same op ([`coil_native_unwind`]).
    pub committed: u64,
    /// The [`TRAP_OVERFLOW`]-style code of the trap a helper hit (`0`: an
    /// operation native code does not model).
    pub trap: u64,
    /// While native code unwinds a panic: the site of each native frame,
    /// innermost first (indexes into [`NativeCode::sites`]).
    pub trace: *mut Vec<u32>,
    /// After a commit native code may recurse past `stack_limit` (it can
    /// no longer bail) down to this address, near the thread's real stack
    /// end; below it the call panics with the VM's stack overflow.
    pub stack_hard: u64,
    /// The message of a [`TRAP_MESSAGE`] panic.
    pub message: *mut String,
    /// The machine running the call, for [`NativeHost`] (null: helpers
    /// that need it bail).
    pub machine: *mut std::ffi::c_void,
    pub host: NativeHost,
    /// VM calls the call made: the VM then checks for a collection.
    pub vm_calls: u64,
    /// The second result word of the last VM call.
    pub vm_hi: u64,
    /// The native frames' heap words while a VM call runs (a collection
    /// may run then): a chain of `[next, n, word × n]` records on the
    /// native stack, newest first, `0`-terminated. The VM pins them.
    pub roots: u64,
    /// Non-zero when a collection is due and the call can no longer bail
    /// for the VM to run it: native code collects at its next safepoint
    /// ([`coil_native_collect`]).
    pub gc_due: u64,
    /// Bumped by every point where a collection may have run (a safepoint
    /// collection, a VM call): native code then reloads its rooted words.
    pub gc_epoch: u64,
}

/// Byte offset of [`NativeCtx::gc_epoch`].
pub const NATIVE_CTX_GC_EPOCH: i32 = std::mem::offset_of!(NativeCtx, gc_epoch) as i32;

/// Byte offset of [`NativeCtx::gc_due`].
pub const NATIVE_CTX_GC_DUE: i32 = std::mem::offset_of!(NativeCtx, gc_due) as i32;

/// Byte offset of [`NativeCtx::roots`].
pub const NATIVE_CTX_ROOTS: i32 = std::mem::offset_of!(NativeCtx, roots) as i32;

impl NativeCtx {
    /// Every heap word the call's native frames hold while it is in a VM
    /// call: the root chain and the queued early frees.
    /// The words are native code's to reload: a moving collection may
    /// rewrite them.
    pub(crate) fn for_each_root(&self, mut visit: impl FnMut(*mut u64)) {
        let mut at = self.roots as *mut u64;
        while !at.is_null() {
            // SAFETY: native code links only live records of its frames.
            unsafe {
                let n = *at.add(1);
                for i in 0..n as usize {
                    visit(at.add(2 + i));
                }
                at = *at as *mut u64;
            }
        }
        if !self.frees.is_null() {
            // SAFETY: `call` set `frees` for the call.
            for (w, _) in unsafe { &mut *self.frees } {
                visit(w);
            }
        }
    }
}

/// Byte offset of [`NativeCtx::vm_hi`].
pub const NATIVE_CTX_VM_HI: i32 = std::mem::offset_of!(NativeCtx, vm_hi) as i32;

/// What native code asks of the machine that runs it, type-erased over its
/// stack size. Each takes [`NativeCtx::machine`].
#[derive(Clone, Copy, Debug)]
pub struct NativeHost {
    /// Write `text` as `PRINT` does.
    pub print: unsafe fn(*mut std::ffi::c_void, &str),
    /// Run the bytecode function at `pc` on `args` and return its result
    /// word; sets the `bool` when it panicked (the VM reported it).
    /// The second result word goes to the last `u64`.
    /// The context is the calling native code's: its roots stay pinned
    /// while the VM runs.
    pub call: unsafe fn(*mut std::ffi::c_void, *mut NativeCtx, u32, &[common::Value], &mut bool) -> u64,
    /// Collect, with the context's roots pinned.
    pub collect: unsafe fn(*mut std::ffi::c_void, *mut NativeCtx),
    /// `FreeEarly` now (no finalizers).
    pub free: unsafe fn(*mut std::ffi::c_void, u64, bool),
}

impl NativeHost {
    const NONE: Self = Self { print: |_, _| {}, call: |_, _, _, _, _| 0, collect: |_, _| {}, free: |_, _, _| {} };
}

/// The machine a native call runs on, as raw pointers: native code and the
/// VM calls it makes reach the machine while the call runs, so `call`
/// holds no Rust borrow of it.
pub(crate) struct NativeEnv {
    pub(crate) heap: *mut crate::memory::Heap,
    pub(crate) strings: *const String,
    pub(crate) string_cache: *mut common::Value,
    pub(crate) strings_len: usize,
    pub(crate) has_finalizers: bool,
    pub(crate) statics: *mut common::Value,
    pub(crate) statics_len: usize,
    pub(crate) machine: *mut std::ffi::c_void,
    pub(crate) host: NativeHost,
}

/// Byte offset of [`NativeCtx::stack_hard`].
pub const NATIVE_CTX_STACK_HARD: i32 = std::mem::offset_of!(NativeCtx, stack_hard) as i32;

/// Byte offset of [`NativeCtx::committed`].
pub const NATIVE_CTX_COMMITTED: i32 = std::mem::offset_of!(NativeCtx, committed) as i32;

/// [`coil_native_unwind`] codes: why native code stopped. `TRAP_HELPER`
/// takes the code a helper left in [`NativeCtx::trap`].
pub const TRAP_HELPER: u64 = 0;
pub const TRAP_OVERFLOW: u64 = 1;
pub const TRAP_DIV_ZERO: u64 = 2;
pub const TRAP_NEGATIVE_EXPONENT: u64 = 3;
pub const TRAP_SHIFT: u64 = 4;
pub const TRAP_OUT_OF_BOUNDS: u64 = 5;
pub const TRAP_STACK: u64 = 6;
/// A panic with the message in [`NativeCtx::message`].
pub const TRAP_MESSAGE: u64 = 7;
/// A VM call native code made panicked; the VM already reported it.
pub const TRAP_VM_PANIC: u64 = 8;
/// An explicit `panic(msg)`: the message in [`NativeCtx::message`].
pub const TRAP_EXPLICIT: u64 = 9;
/// No site: a frame that pushes nothing onto the panic's trace.
pub const NO_SITE: u64 = u32::MAX as u64;

/// [`NativeCtx::bail`] of a call that panicked natively (`1`: bailed).
pub const BAIL_PANIC: u64 = 2;

/// Byte offsets of [`NativeCtx::statics`] and [`NativeCtx::statics_len`].
pub const NATIVE_CTX_STATICS: i32 = std::mem::offset_of!(NativeCtx, statics) as i32;
pub const NATIVE_CTX_STATICS_LEN: i32 = std::mem::offset_of!(NativeCtx, statics_len) as i32;

/// Byte offset of [`NativeCtx::ret_hi`].
pub const NATIVE_CTX_RET_HI: i32 = std::mem::offset_of!(NativeCtx, ret_hi) as i32;

/// Allocations one native call may make while a collection is due.
pub const NATIVE_ALLOC_CAP: u64 = 1 << 16;

/// One heap store native code made: `[target, index, old word]`. `target`
/// is the object's address, with bit 0 set for an instance slot (clear for
/// an array element). A bail restores them newest first, so the VM's re-run
/// starts from the heap as the call found it.
pub type UndoEntry = [u64; 3];

/// [`UndoEntry`] target bit of an instance slot store.
pub const UNDO_FIELD: u64 = 1;
/// [`UndoEntry`] target bit of an array push (undone by a pop).
pub const UNDO_PUSH: u64 = 2;
/// [`UndoEntry`] target of a static store (no object): `[UNDO_STATIC,
/// slot, old]`.
pub const UNDO_STATIC: u64 = 4;

/// Byte offset of [`NativeCtx::bail`].
pub const NATIVE_CTX_BAIL: i32 = 0;
/// Byte offset of [`NativeCtx::stack_limit`].
pub const NATIVE_CTX_STACK_LIMIT: i32 = 8;
/// Byte offsets of the undo log fields of [`NativeCtx`].
pub const NATIVE_CTX_UNDO_BUF: i32 = 16;
pub const NATIVE_CTX_UNDO_LEN: i32 = 24;
pub const NATIVE_CTX_UNDO_CAP: i32 = 32;

impl NativeCtx {
    /// A context for one call from the VM, logging into `undo`.
    pub fn new(stack_limit: u64, undo: &mut Vec<UndoEntry>) -> Self {
        undo.clear();
        Self {
            bail: 0,
            stack_limit,
            undo_buf: undo.as_mut_ptr(),
            undo_len: 0,
            undo_cap: undo.capacity() as u64,
            undo_vec: undo,
            heap: std::ptr::null_mut(),
            allocs: 0,
            ret_hi: 0,
            strings: std::ptr::null(),
            string_cache: std::ptr::null_mut(),
            strings_len: 0,
            frees: std::ptr::null_mut(),
            has_finalizers: 0,
            statics: std::ptr::null_mut(),
            statics_len: 0,
            committed: 0,
            trap: 0,
            trace: std::ptr::null_mut(),
            stack_hard: stack_limit,
            message: std::ptr::null_mut(),
            machine: std::ptr::null_mut(),
            host: NativeHost::NONE,
            vm_calls: 0,
            vm_hi: 0,
            roots: 0,
            gc_due: 0,
            gc_epoch: 0,
        }
    }

    /// As [`Self::new`], with `heap` for the helpers that allocate.
    pub fn with_heap(stack_limit: u64, undo: &mut Vec<UndoEntry>, heap: *mut crate::memory::Heap) -> Self {
        Self { heap, ..Self::new(stack_limit, undo) }
    }

    /// Put back every logged store, newest first.
    pub fn undo(&mut self) {
        for i in (0..self.undo_len as usize).rev() {
            // SAFETY: entries below `undo_len` were written.
            let [target, index, old] = unsafe { *self.undo_buf.add(i) };
            let old = common::Value::from(old);
            if target == UNDO_STATIC {
                if index < self.statics_len {
                    // SAFETY: `index < statics_len`.
                    unsafe { *self.statics.add(index as usize) = old };
                }
            } else if target & UNDO_PUSH != 0 {
                if let Some(crate::memory::Object::Array(mut gc)) = object(target & !UNDO_PUSH) {
                    let _ = gc.as_mut().pop();
                }
            } else if target & UNDO_FIELD != 0 {
                if let Some(crate::memory::Object::Instance(mut gc)) = object(target & !UNDO_FIELD) {
                    gc.as_mut().set_slot(index as usize, old);
                }
            } else if let Some(crate::memory::Object::Array(mut gc)) = object(target) {
                gc.as_mut().store_indexed(index as i64, old, false);
            }
        }
        self.undo_len = 0;
    }
}

/// Make room for one more undo entry.
///
/// # Safety
/// `ctx` is a live [`NativeCtx`] made by [`NativeCtx::new`].
pub unsafe extern "C" fn coil_native_undo_grow(ctx: *mut NativeCtx) {
    let ctx = unsafe { &mut *ctx };
    // A committed call never bails, so nothing will be undone: start over
    // (once there is room for an entry).
    if ctx.committed != 0 && ctx.undo_cap > 0 {
        ctx.undo_len = 0;
        return;
    }
    let vec = unsafe { &mut *ctx.undo_vec };
    // SAFETY: the first `undo_len` entries are initialized.
    unsafe { vec.set_len(ctx.undo_len as usize) };
    vec.reserve(vec.len().max(64));
    ctx.undo_buf = vec.as_mut_ptr();
    ctx.undo_cap = vec.capacity() as u64;
}

/// How much native stack a call from the VM may use before it bails to the
/// VM (which then runs the call with its own frame limits).
pub const NATIVE_STACK_BUDGET: u64 = 256 * 1024;

/// Stack native code leaves unused at the thread's end after a commit
/// ([`NativeCtx::stack_hard`]).
pub const NATIVE_STACK_GUARD: u64 = 128 * 1024;

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
    /// Result words: `1`, or `2` for a two-slot return (`lo`, then `hi`
    /// in [`NativeCtx::ret_hi`]), as the VM's `RETURN` pushes them.
    pub ret_words: u8,
    pub entry: NativeEntryFn,
}

/// An op native code may panic at: the function it is in (by entry PC)
/// and the op's source location. A native panic's trace names one per
/// native frame, which the VM prints as it prints its own frames.
#[derive(Clone, Copy, Debug)]
pub struct NativeSite {
    pub entry_pc: u32,
    pub loc: common::DebugLoc,
}

/// The compiled functions of one program, indexed by entry PC. Shared
/// (`Arc`) by every machine that runs the program.
#[derive(Default)]
pub struct NativeCode {
    /// `by_pc[pc]` is `index + 1` into `entries`, or `0`.
    by_pc: Vec<u32>,
    entries: Vec<NativeEntry>,
    sites: Vec<NativeSite>,
}

impl NativeCode {
    /// `code_len` is the program's bytecode length; entries past it are
    /// dropped. `sites` are the panic sites the code names.
    pub fn new(code_len: usize, entries: Vec<NativeEntry>, sites: Vec<NativeSite>) -> Self {
        let mut by_pc = vec![0u32; code_len];
        let mut kept = Vec::with_capacity(entries.len());
        for e in entries {
            if let Some(slot) = by_pc.get_mut(e.pc as usize) {
                kept.push(e);
                *slot = kept.len() as u32;
            }
        }
        Self { by_pc, entries: kept, sites }
    }

    pub(crate) fn site(&self, i: u32) -> Option<NativeSite> {
        self.sites.get(i as usize).copied()
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

/// What a native call gave back: its result words and whether it
/// allocated (the VM then checks for a collection).
pub(crate) struct NativeResult {
    pub(crate) lo: u64,
    pub(crate) hi: Option<u64>,
    pub(crate) allocated: bool,
    /// Early frees to make now, in order (see [`NativeCtx::frees`]).
    pub(crate) frees: Vec<(u64, bool)>,
}

/// A panic native code raised after it committed: the [`TRAP_OVERFLOW`]
/// style code and the native frames' sites, innermost first.
pub(crate) struct NativePanic {
    pub(crate) code: u64,
    pub(crate) trace: Vec<u32>,
    /// The text of a [`TRAP_MESSAGE`] panic.
    pub(crate) message: String,
}

impl NativePanic {
    /// The VM's message for the same trap.
    pub(crate) fn message(&self) -> &str {
        match self.code {
            TRAP_MESSAGE | TRAP_EXPLICIT => &self.message,
            TRAP_OVERFLOW => common::int_arith::IntTrap::Overflow.message(),
            TRAP_DIV_ZERO => common::int_arith::IntTrap::DivByZero.message(),
            TRAP_NEGATIVE_EXPONENT => common::int_arith::IntTrap::NegativeExponent.message(),
            TRAP_SHIFT => common::int_arith::IntTrap::ShiftOutOfRange.message(),
            TRAP_OUT_OF_BOUNDS => "index out of bounds",
            TRAP_STACK => crate::vm::STACK_OVERFLOW,
            _ => "native code cannot finish this call after its effects (an operation it does not model)",
        }
    }
}

/// How a native call ended.
pub(crate) enum NativeOutcome {
    Done(NativeResult),
    /// Bailed: the VM runs the call again.
    Bailed,
    Panicked(NativePanic),
}

/// Run `entry` with `args`.
#[inline(never)]
pub(crate) fn call(entry: &NativeEntry, args: &[u64], env: &NativeEnv) -> NativeOutcome {
    let marker = 0u8;
    let sp = std::ptr::addr_of!(marker) as u64;
    let mut undo = Vec::new();
    let mut ctx = NativeCtx::with_heap(sp.saturating_sub(NATIVE_STACK_BUDGET), &mut undo, env.heap);
    if let Some(left) = stacker::remaining_stack() {
        ctx.stack_hard = ctx.stack_hard.min(sp.saturating_sub((left as u64).saturating_sub(NATIVE_STACK_GUARD)));
    }
    ctx.machine = env.machine;
    ctx.host = env.host;
    ctx.strings = env.strings;
    ctx.string_cache = env.string_cache;
    ctx.strings_len = env.strings_len as u64;
    let mut frees = Vec::new();
    ctx.frees = &mut frees;
    ctx.has_finalizers = u64::from(env.has_finalizers);
    ctx.statics = env.statics;
    ctx.statics_len = env.statics_len as u64;
    let mut trace = Vec::new();
    ctx.trace = &mut trace;
    let mut message = String::new();
    ctx.message = &mut message;
    // SAFETY: the backend compiled `entry` for exactly `arity` argument
    // words, which `args` holds; `ctx` outlives the call.
    let out = unsafe { (entry.entry)(&mut ctx, args.as_ptr()) };
    match ctx.bail {
        0 => {
            let hi = (entry.ret_words == 2).then_some(ctx.ret_hi);
            NativeOutcome::Done(NativeResult { lo: out, hi, allocated: ctx.allocs != 0 || ctx.vm_calls != 0, frees })
        }
        // Its effects stay, as a VM panic's do.
        BAIL_PANIC => NativeOutcome::Panicked(NativePanic { code: ctx.trap, trace, message }),
        _ => {
            ctx.undo();
            NativeOutcome::Bailed
        }
    }
}

/// Where native code stops a call: called by every frame on the way out,
/// with the frame's `site` ([`NO_SITE`] for none) and the reason `code`.
/// Before the call committed it bails (the VM runs the call again); after,
/// the first frame raises a panic and every frame adds its site to the
/// trace.
///
/// # Safety
/// `ctx` is the call's live [`NativeCtx`].
pub unsafe extern "C" fn coil_native_unwind(ctx: *mut NativeCtx, code: u64, site: u64) {
    let ctx = unsafe { &mut *ctx };
    if ctx.bail != BAIL_PANIC {
        if ctx.committed == 0 {
            ctx.bail = 1;
            return;
        }
        if code != TRAP_HELPER {
            ctx.trap = code;
        }
        ctx.bail = BAIL_PANIC;
    }
    if site != NO_SITE {
        // SAFETY: `call` set `trace` for the call.
        unsafe { (*ctx.trace).push(site as u32) };
    }
}

thread_local! {
    /// Per thread: a harness compiles and runs several programs at once.
    static PENDING: std::cell::RefCell<Option<Pending>> = const { std::cell::RefCell::new(None) };
}

/// Native code for the next [`crate::Machine::run_raw`] on this thread
/// (the program's main machine); the machines it spawns share it.
pub fn install_for_next_run(entries: Vec<NativeEntry>, sites: Vec<NativeSite>) {
    PENDING.with(|p| *p.borrow_mut() = Some(Pending::Entries(entries, sites)));
}

/// [`install_for_next_run`] with code already built for the program.
pub fn install_shared_for_next_run(code: std::sync::Arc<NativeCode>) {
    PENDING.with(|p| *p.borrow_mut() = Some(Pending::Shared(code)));
}

pub(crate) enum Pending {
    Entries(Vec<NativeEntry>, Vec<NativeSite>),
    Shared(std::sync::Arc<NativeCode>),
}

impl Pending {
    pub(crate) fn into_code(self, code_len: usize) -> std::sync::Arc<NativeCode> {
        match self {
            Self::Entries(entries, sites) => std::sync::Arc::new(NativeCode::new(code_len, entries, sites)),
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
// re-enters the VM and its helpers never collect (the Cow helpers below
// allocate, but the VM collects only after the call returns), so every
// address it holds stays a live object at the same place. Each helper does what
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
            unsafe {
                if matches!(object(addr), Some(crate::memory::Object::Array(_) | crate::memory::Object::Tuple(_))) {
                    (*ctx).trap = TRAP_OUT_OF_BOUNDS;
                }
                bail(ctx)
            }
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

/// Log a store into `ctx`'s undo log.
///
/// # Safety
/// `ctx` is a live [`NativeCtx`] made by [`NativeCtx::new`].
unsafe fn log(ctx: *mut NativeCtx, entry: UndoEntry) {
    unsafe {
        if (*ctx).undo_len == (*ctx).undo_cap {
            coil_native_undo_grow(ctx);
        }
        let c = &mut *ctx;
        c.undo_buf.add(c.undo_len as usize).write(entry);
        c.undo_len += 1;
    }
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
        unsafe { (*ctx).trap = TRAP_OUT_OF_BOUNDS };
        return unsafe { bail(ctx) };
    };
    unsafe { log(ctx, [addr, index as u64, old.bits()]) };
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
    unsafe { log(ctx, [addr | UNDO_FIELD, slot as u64, old.bits()]) };
    gc.as_mut().set_slot(slot, common::Value::from(value));
    value
}

/// An array's element words, for native code to index in place: writes
/// `[pointer, length]` to `out` and returns `1` for an array that stores
/// words, `0` for anything else (a byte array, a tuple, not an array),
/// which native code then reads through the helpers above. With
/// `for_store` set, marks the array as possibly holding references, as
/// every VM store does. The pointer stays valid while native code runs:
/// it never grows or shrinks an array, and storing into a word array never
/// moves its storage.
///
/// # Safety
/// `addr` is a heap word; `out` points at two writable words.
pub unsafe extern "C" fn coil_native_view(addr: u64, for_store: u64, out: *mut u64) -> u64 {
    let Some(crate::memory::Object::Array(mut gc)) = object(addr) else {
        return 0;
    };
    match gc.as_mut().words_view(for_store != 0) {
        Some((ptr, len)) => {
            unsafe {
                out.write(ptr as u64);
                out.add(1).write(len as u64);
            }
            1
        }
        None => 0,
    }
}

// ---------------------------------------------------------------------------
// Copy-on-write for native code. These allocate (a unique copy) but never
// collect; see [`NativeCtx::allocs`].

/// The heap of `ctx`, or `None` (the caller bails).
unsafe fn heap<'a>(ctx: *mut NativeCtx) -> Option<&'a mut crate::memory::Heap> {
    unsafe { (*ctx).heap.as_mut() }
}

/// Count one allocation; bail when the call allocated past
/// [`NATIVE_ALLOC_CAP`] with a collection due.
unsafe fn note_alloc(ctx: *mut NativeCtx, heap: &crate::memory::Heap) {
    let ctx = unsafe { &mut *ctx };
    ctx.allocs += 1;
    if ctx.committed != 0 {
        // The call cannot bail any more: it collects at a safepoint.
        if !heap.gc_is_idle() || heap.should_collect() {
            ctx.gc_due = 1;
        }
    } else if ctx.allocs >= NATIVE_ALLOC_CAP && heap.should_collect() {
        ctx.bail = 1;
    }
}

/// A safepoint found [`NativeCtx::gc_due`]: native code linked its heap
/// words into [`NativeCtx::roots`]; the VM collects with them pinned.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`].
pub unsafe extern "C" fn coil_native_collect(ctx: *mut NativeCtx) {
    let c = unsafe { &mut *ctx };
    c.gc_due = 0;
    if c.machine.is_null() {
        return;
    }
    c.gc_epoch += 1;
    let (machine, collect) = (c.machine, c.host.collect);
    // SAFETY: `machine` is the live machine `host` was made for.
    unsafe { collect(machine, ctx) };
}

/// `Cow` without a field: share `word` (`unique == 0`) or return a unique
/// copy of it when it is shared (`unique != 0`), as the VM's `Cow` does.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `word` is a heap word.
pub unsafe extern "C" fn coil_native_cow(ctx: *mut NativeCtx, word: u64, unique: u64) -> u64 {
    if crate::vm::cow_eager() {
        return unsafe { bail(ctx) };
    }
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    let v = common::Value::from(word);
    if unique == 0 {
        heap.cow_share(v);
        return word;
    }
    match heap.cow_unique(v) {
        Some(copy) => {
            unsafe { note_alloc(ctx, heap) };
            copy.bits()
        }
        None => word,
    }
}

/// `Cow` of field `field` of the instance `obj`: `obj.f = unique(obj.f)`,
/// writing the slot back (and logging it for undo) only when it copied.
/// Returns `obj`.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `obj` is a heap word.
pub unsafe extern "C" fn coil_native_cow_field(ctx: *mut NativeCtx, obj: u64, field: u64) -> u64 {
    if crate::vm::cow_eager() {
        return unsafe { bail(ctx) };
    }
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    if let Some(crate::memory::Object::Instance(mut gc)) = object(obj)
        && let Some(v) = gc.as_ref().slot(field as usize)
        && let Some(copy) = heap.cow_unique(v)
    {
        unsafe { log(ctx, [obj | UNDO_FIELD, field, v.bits()]) };
        gc.as_mut().set_slot(field as usize, copy);
        unsafe { note_alloc(ctx, heap) };
    }
    obj
}

// ---------------------------------------------------------------------------
// Allocation for native code (`Alloc`, `ArrayPush`), as the VM's
// `DenseMake` / `DenseMakeObject` / `ArrayPush` do. Same rule as the Cow
// helpers: they allocate and never collect.

/// [`coil_native_alloc`] kinds.
pub const ALLOC_ARRAY: u64 = 0;
pub const ALLOC_TUPLE: u64 = 1;
pub const ALLOC_ENUM: u64 = 2;
pub const ALLOC_OBJECT: u64 = 3;

/// A new object of `kind` from the `n` words at `words`, in declaration
/// order (an enum's payload order):
/// - [`ALLOC_ARRAY`]: `extra` is the element word kind (`0`: none);
/// - [`ALLOC_TUPLE`]: `extra` is the packed word kinds;
/// - [`ALLOC_ENUM`]: `extra` is `tag | kinds << 32`;
/// - [`ALLOC_OBJECT`]: `extra` is the type id and `n` the field count
///   (`words` unused: stores fill the fields).
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `words` points at `n` words.
pub unsafe extern "C" fn coil_native_alloc(ctx: *mut NativeCtx, kind: u64, words: *const u64, n: u64, extra: u64) -> u64 {
    use crate::memory::{EnumPayload, ObjArray, ObjEnum, ObjInstance, ObjTuple, Object};
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    let n = n as usize;
    let values: Vec<common::Value> = if kind == ALLOC_OBJECT || n == 0 {
        Vec::new()
    } else {
        // SAFETY: the caller passes `n` words.
        unsafe { std::slice::from_raw_parts(words, n) }.iter().map(|&w| common::Value::from(w)).collect()
    };
    let addr = match kind {
        ALLOC_ARRAY => {
            let mut arr = ObjArray::from_values(values, heap);
            if extra != 0 {
                arr.set_elem_kind(extra as u8 & 0x3);
            }
            heap.alloc(arr, Object::Array).0.addr()
        }
        ALLOC_TUPLE if n == 0 => return heap.immortal_empty_tuple().addr(),
        ALLOC_TUPLE => heap.alloc(ObjTuple::from_slice(&values).with_kinds(extra as u8), Object::Tuple).0.addr(),
        ALLOC_ENUM => {
            let tag = extra as u32;
            if n == 0 {
                // A payload-less variant is shared, as `MakeEnum`.
                return heap.immortal_unit_enum(tag).addr();
            }
            let payload = match values.as_slice() {
                [a] => EnumPayload::one(*a),
                [a, b] => EnumPayload::two(*a, *b),
                v => EnumPayload::from_slice(v),
            }
            .with_kinds((extra >> 32) as u8);
            heap.alloc(ObjEnum::new(tag, payload), Object::Enum).0.addr()
        }
        ALLOC_OBJECT => {
            let (_, mut r) = heap.alloc(ObjInstance::with_type_id_and_fields(extra as u32, n), Object::Instance);
            let _ = r.as_mut();
            r.as_ptr().addr() as u64
        }
        _ => return unsafe { bail(ctx) },
    };
    unsafe { note_alloc(ctx, heap) };
    addr
}

/// `ArrayPush`: append `value` to the array `addr` (logged for undo as a
/// pop) and return `addr`. Not an array: bail, where the VM panics.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `addr` is a heap word.
pub unsafe extern "C" fn coil_native_array_push(ctx: *mut NativeCtx, addr: u64, value: u64) -> u64 {
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    let Some(crate::memory::Object::Array(mut gc)) = object(addr) else {
        return unsafe { bail(ctx) };
    };
    unsafe { log(ctx, [addr | UNDO_PUSH, 0, 0]) };
    gc.as_mut().push(common::Value::from(value));
    heap.account_array(gc.as_mut());
    unsafe { note_alloc(ctx, heap) };
    addr
}

// ---------------------------------------------------------------------------
// Boxed enum matches (`JumpIfMatch`, `MatchPayload` on an enum object).

/// [`coil_native_enum_tag`] of a word that is not an enum object.
pub const NOT_AN_ENUM: u64 = u64::MAX;

/// An enum object's `tag | payload length << 32`, or [`NOT_AN_ENUM`].
///
/// # Safety
/// `addr` is a heap word.
pub unsafe extern "C" fn coil_native_enum_tag(addr: u64) -> u64 {
    match object(addr) {
        Some(crate::memory::Object::Enum(gc)) => {
            let e = gc.as_ref();
            u64::from(e.tag) | ((e.payload.len() as u64) << 32)
        }
        _ => NOT_AN_ENUM,
    }
}

/// Payload word `index` (declaration order) of the enum object `addr`.
/// Not an enum or out of range: bail.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `addr` is a heap word.
pub unsafe extern "C" fn coil_native_enum_payload(ctx: *mut NativeCtx, addr: u64, index: u64) -> u64 {
    match object(addr) {
        Some(crate::memory::Object::Enum(gc)) => match gc.as_ref().payload.get(index as usize) {
            Some(v) => v.bits(),
            None => unsafe { bail(ctx) },
        },
        _ => unsafe { bail(ctx) },
    }
}

/// `STRING idx`: the program string's interned object, as the VM's
/// `push_program_string` (cached per machine). Bad index: bail.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`].
pub unsafe extern "C" fn coil_native_string(ctx: *mut NativeCtx, idx: u64) -> u64 {
    let c = unsafe { &mut *ctx };
    if idx >= c.strings_len {
        return unsafe { bail(ctx) };
    }
    // SAFETY: `idx < strings_len`, the length of both tables.
    let cached = unsafe { *c.string_cache.add(idx as usize) };
    if !cached.is_zero() {
        return cached.bits();
    }
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    let data = unsafe { &*c.strings.add(idx as usize) };
    let handle = common::Value::from(heap.intern_str(data).as_ptr() as *mut u8 as u64);
    // No collection runs during native code, so the cache entry stays
    // valid until the VM's next sweep clears it, as after `STRING`.
    unsafe { *c.string_cache.add(idx as usize) = handle };
    unsafe { note_alloc(ctx, heap) };
    handle.bits()
}

/// `StrEq` (the VM's `EQ`): `1` when `a` and `b` are equal as the VM
/// compares them (strings by content, other objects structurally).
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `a` and `b` are words.
pub unsafe extern "C" fn coil_native_eq(ctx: *mut NativeCtx, a: u64, b: u64) -> u64 {
    if a == b {
        return 1;
    }
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    let (a, b) = (common::Value::from(a), common::Value::from(b));
    if !heap.may_be_ref(a) || !heap.may_be_ref(b) {
        return 0;
    }
    u64::from(crate::value_eq::values_eq(heap, a, b))
}

/// `STRINGIFY`: a new string of `word`, as the VM writes it. `tag` is the
/// instruction's operand (`common::stringify_operand`, `0`: inspect the
/// word).
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`].
pub unsafe extern "C" fn coil_native_stringify(ctx: *mut NativeCtx, word: u64, tag: u64) -> u64 {
    type M = crate::Machine<{ crate::DEFAULT_OPERAND_STACK_SLOTS }>;
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    let v = common::Value::from(word);
    let text = match common::stringify_tag(tag as u32) {
        Some(t) => M::stringify_immediate(t, v),
        None => M::stringify_value(heap, v),
    };
    let s = heap.alloc_string(text);
    unsafe { note_alloc(ctx, heap) };
    s.as_ptr() as *mut u8 as u64
}

/// `FORMAT` with the format string `fmt` and the `n` argument words at
/// `args` (in order), as the VM's handler: a leading `%s` appends the rest
/// to the first argument (in place when that string ends at its buffer's
/// tail), and with no arguments only `%%` is rewritten.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `fmt` is a string object; `args`
/// points at `n` words.
pub unsafe extern "C" fn coil_native_format(ctx: *mut NativeCtx, fmt: u64, args: *const u64, n: u64) -> u64 {
    use crate::memory::{GcData, ObjString, Object};
    type M = crate::Machine<{ crate::DEFAULT_OPERAND_STACK_SLOTS }>;
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    let Some(Object::String(_)) = object(fmt) else {
        return unsafe { bail(ctx) };
    };
    // SAFETY: checked above.
    let format_string = unsafe { &*common::Value::from(fmt).as_ptr::<GcData<ObjString>>() }.as_ref().data.as_str();
    let words: &[u64] = if n == 0 { &[] } else { unsafe { std::slice::from_raw_parts(args, n as usize) } };
    let mut words = words.iter().map(|&w| common::Value::from(w));
    let addr = if n == 0 {
        if !format_string.contains("%%") {
            return fmt;
        }
        let message = format_string.replace("%%", "%");
        heap.alloc_string(message).as_ptr() as *mut u8 as u64
    } else {
        let (head, rest) = match format_string.strip_prefix("%s") {
            Some(rest) => (words.next(), rest),
            None => (None, format_string),
        };
        let message = crate::vm::render_format(rest, || words.next().unwrap_or_default());
        match head {
            Some(head) => {
                let joined = match object(head.bits()) {
                    Some(Object::String(gh)) => gh.as_ref().concat(&message),
                    _ => ObjString::new(M::object_string_value(heap, &head) + &message),
                };
                heap.alloc(joined, Object::String).0.addr()
            }
            None => heap.alloc_string(message).as_ptr() as *mut u8 as u64,
        }
    };
    unsafe { note_alloc(ctx, heap) };
    addr
}

/// `FreeEarly`: queue `word` for the VM to free after the call (see
/// [`NativeCtx::frees`]); once the call committed (it can no longer bail)
/// the VM frees it now. With finalizers in the program: bail.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`].
pub unsafe extern "C" fn coil_native_free_early(ctx: *mut NativeCtx, word: u64, deep: u64) -> u64 {
    let c = unsafe { &mut *ctx };
    if c.has_finalizers != 0 || c.frees.is_null() {
        return unsafe { bail(ctx) };
    }
    if c.committed != 0 && !c.machine.is_null() {
        // SAFETY: `machine` is the live machine `host` was made for.
        unsafe { (c.host.free)(c.machine, word, deep != 0) };
        return 0;
    }
    unsafe { (*c.frees).push((word, deep != 0)) };
    0
}

/// `PRINT` of the string `word`. A print cannot be taken back: the call
/// commits ([`NativeCtx::committed`]) first.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `word` is a heap word.
pub unsafe extern "C" fn coil_native_print(ctx: *mut NativeCtx, word: u64) -> u64 {
    let c = unsafe { &mut *ctx };
    let Some(crate::memory::Object::String(gc)) = object(word) else {
        return unsafe { bail(ctx) };
    };
    if c.machine.is_null() {
        return unsafe { bail(ctx) };
    }
    c.committed = 1;
    // SAFETY: `machine` is the live machine `host` was made for.
    unsafe { (c.host.print)(c.machine, gc.as_ref().data.as_str()) };
    0
}

/// A call from native code to a body that runs on the VM: the bytecode
/// function at `pc` on the `n` words at `args`. What the VM runs cannot be
/// taken back, so the call commits first. When the VM's run panicked (it
/// reported the panic), native code unwinds with [`TRAP_VM_PANIC`] and the
/// VM goes on unwinding its own frames.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `args` points at `n` words.
pub unsafe extern "C" fn coil_native_call_vm(ctx: *mut NativeCtx, pc: u64, args: *const u64, n: u64) -> u64 {
    let c = unsafe { &mut *ctx };
    let mut buf = [common::Value::default(); 16];
    let Some(words) = buf.get_mut(..n as usize) else {
        return unsafe { bail(ctx) };
    };
    if c.machine.is_null() {
        return unsafe { bail(ctx) };
    }
    for (i, w) in words.iter_mut().enumerate() {
        // SAFETY: `args` holds `n` words.
        *w = common::Value::from(unsafe { *args.add(i) });
    }
    c.committed = 1;
    c.vm_calls += 1;
    c.gc_epoch += 1;
    let mut panicked = false;
    // SAFETY: `machine` is the live machine `host` was made for.
    let (machine, call) = (c.machine, c.host.call);
    let out = unsafe { call(machine, ctx, pc as u32, words, &mut panicked) };
    let c = unsafe { &mut *ctx };
    if panicked {
        c.trap = TRAP_VM_PANIC;
        return unsafe { bail(ctx) };
    }
    out
}

/// The standard host natives native code may call straight
/// ([`crate::ffi::NativeFn::direct`]), by id, with their names: the ids are
/// frozen (`common::host`), so code compiled against this table calls the
/// same functions every machine registers.
pub fn direct_host_natives() -> &'static [DirectHost] {
    static TABLE: std::sync::OnceLock<Vec<DirectHost>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        crate::host_natives::build_standard_host_natives(|_, _| {})
            .iter()
            .map(|n| {
                let guarded = crate::host_natives::stream_first(n.name());
                let func = if guarded { n.guarded_direct() } else { n.direct() };
                DirectHost { name: n.name().to_string(), func, guarded }
            })
            .collect()
    })
}

/// One entry of [`direct_host_natives`].
pub struct DirectHost {
    pub name: String,
    /// The plain function (`None`: the native needs the VM).
    pub func: Option<crate::HostValueFn>,
    /// It takes a `Stream` first, which must not be unsent.
    pub guarded: bool,
}

/// `HostInvoke` of the direct host native `id` ([`direct_host_natives`])
/// on the `n` words at `args`, constructing enums in `layout`
/// (`common::HOST_ENUM_LAYOUT_*`), as the VM's direct path does. The
/// native's effects cannot be taken back: the call commits first.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `args` points at `n` words.
pub unsafe extern "C" fn coil_native_host(ctx: *mut NativeCtx, id: u64, layout: u64, args: *const u64, n: u64) -> u64 {
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    let c = unsafe { &mut *ctx };
    let Some(DirectHost { name, func: Some(f), guarded }) = direct_host_natives().get(id as usize) else {
        return unsafe { bail(ctx) };
    };
    let mut buf = [common::Value::default(); 16];
    let Some(words) = buf.get_mut(..n as usize) else {
        return unsafe { bail(ctx) };
    };
    for (i, w) in words.iter_mut().enumerate() {
        // SAFETY: `args` holds `n` words.
        *w = common::Value::from(unsafe { *args.add(i) });
    }
    let args = &*words;
    if *guarded && args.first().is_some_and(|&s| crate::io::is_unsent_stream(heap, s)) {
        c.trap = TRAP_MESSAGE;
        // SAFETY: `call` set `message` for the call.
        unsafe { *c.message = format!("{name}: {}", crate::io::UNSENT_STREAM_PANIC) };
        return unsafe { bail(ctx) };
    }
    c.committed = 1;
    let live_before = heap.live_object_count();
    let layout = crate::host_enum::HostEnumLayout::from_u32(layout as u32);
    let v = crate::host_enum::in_host_enum_layout(layout, || f(heap, args));
    // The VM checks for a collection once the call returns.
    let made = heap.live_object_count().saturating_sub(live_before) as u64;
    if made != 0 {
        c.allocs += made - 1;
        unsafe { note_alloc(ctx, heap) };
    }
    v.bits()
}

/// `Panic` of the string `msg`: before a commit, bail (the VM's run panics);
/// after, a native panic with that message ([`TRAP_EXPLICIT`]).
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `msg` is a heap word.
pub unsafe extern "C" fn coil_native_panic(ctx: *mut NativeCtx, msg: u64) -> u64 {
    let c = unsafe { &mut *ctx };
    if let Some(crate::memory::Object::String(gc)) = object(msg) {
        c.trap = TRAP_EXPLICIT;
        // SAFETY: `call` set `message` for the call.
        unsafe { *c.message = gc.as_ref().data.as_str().to_string() };
    }
    unsafe { bail(ctx) }
}
