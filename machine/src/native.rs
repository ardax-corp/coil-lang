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
    /// The machine's heap, for helpers that allocate (null: they bail).
    pub heap: *mut crate::memory::Heap,
    /// Objects helpers allocated during this call. No collection runs
    /// while native code holds raw heap words, so the VM collects after
    /// the call returns; past [`NATIVE_ALLOC_CAP`] with a collection due,
    /// the call commits and collects at its next safepoint.
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
    /// Cleanup pads of the frames a committed panic unwound, innermost
    /// first ([`coil_native_defer`]): the VM runs them once it reported
    /// the panic, as it runs its own frames' pads.
    pub pads: *mut Vec<NativePad>,
    /// While a native frame makes a call that may run VM code (which may
    /// panic and print a backtrace): a chain of `[next, site]` records on
    /// the native stack, innermost first, `0`-terminated, naming each such
    /// frame's call site ([`NativeCode::sites`]).
    pub calls: u64,
}

/// A `defer` cleanup pad: called with its frame's homes.
pub type NativePadFn = unsafe extern "C" fn(ctx: *mut NativeCtx, homes: *const u64) -> u64;

/// The pad of one native frame a panic unwound, with a copy of its homes
/// laid out as a masked root record (`[next, n | ROOTS_MASKED, mask,
/// words...]`, see [`NativeCtx::roots`]): a collection while pads run
/// keeps (and may move) the heap words among them.
pub struct NativePad {
    pub func: NativePadFn,
    pub homes: Vec<u64>,
}

/// Set in a root record's count: the next word is a mask of which words
/// are heap words (the rest are scalars to skip).
pub const ROOTS_MASKED: u64 = 1 << 63;

/// Set in a root record's count: its words are ambiguous (an opaque word
/// of a generic body may be an immediate), so the collector pins what they
/// hit instead of moving it, and never rewrites them.
pub const ROOTS_AMBIGUOUS: u64 = 1 << 62;

/// Byte offset of [`NativeCtx::gc_epoch`].
pub const NATIVE_CTX_GC_EPOCH: i32 = std::mem::offset_of!(NativeCtx, gc_epoch) as i32;

/// Byte offset of [`NativeCtx::gc_due`].
pub const NATIVE_CTX_GC_DUE: i32 = std::mem::offset_of!(NativeCtx, gc_due) as i32;
/// Byte offset of [`NativeCtx::has_finalizers`].
pub const NATIVE_CTX_HAS_FINALIZERS: i32 = std::mem::offset_of!(NativeCtx, has_finalizers) as i32;

/// Byte offset of [`NativeCtx::calls`].
pub const NATIVE_CTX_CALLS: i32 = std::mem::offset_of!(NativeCtx, calls) as i32;

/// Byte offset of [`NativeCtx::roots`].
pub const NATIVE_CTX_ROOTS: i32 = std::mem::offset_of!(NativeCtx, roots) as i32;

impl NativeCtx {
    /// Every heap word the call's native frames hold while it is in a VM
    /// call: the root chain and the queued early frees.
    /// The words are native code's to reload: a moving collection may
    /// rewrite them.
    /// The sites of [`Self::calls`], innermost first.
    pub(crate) fn call_sites(&self) -> Vec<u32> {
        let mut out = Vec::new();
        let mut at = self.calls as *const u64;
        while !at.is_null() {
            // SAFETY: native code links only live records of its frames.
            unsafe {
                out.push(*at.add(1) as u32);
                at = *at as *const u64;
            }
        }
        out
    }

    pub(crate) fn for_each_root(&self, mut visit: impl FnMut(*mut u64, bool)) {
        let mut at = self.roots as *mut u64;
        while !at.is_null() {
            // SAFETY: native code links only live records of its frames.
            unsafe {
                let n = *at.add(1);
                let ambiguous = n & ROOTS_AMBIGUOUS != 0;
                let count = n & !(ROOTS_MASKED | ROOTS_AMBIGUOUS);
                if n & ROOTS_MASKED != 0 {
                    let mask = *at.add(2);
                    for i in 0..count.min(64) as usize {
                        if mask & (1 << i) != 0 {
                            visit(at.add(3 + i), ambiguous);
                        }
                    }
                } else {
                    for i in 0..count as usize {
                        visit(at.add(2 + i), ambiguous);
                    }
                }
                at = *at as *mut u64;
            }
        }
        if !self.frees.is_null() {
            // SAFETY: `call` set `frees` for the call.
            for (w, _) in unsafe { &mut *self.frees } {
                visit(w, false);
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
    /// Call the function value (or code pointer) word on `args`, the last
    /// `u32` of them instance dictionaries, as `CallIndirect` does; same
    /// result and panic rules as `call`.
    pub call_value: unsafe fn(*mut std::ffi::c_void, *mut NativeCtx, u64, &[common::Value], u32, &mut bool) -> u64,
    /// Before a call of the body at `pc` on `word`: `1` when `pc` is no
    /// `drop`; else, as the VM's `CALL` claims a finalizer, `2` when this
    /// call claimed it and `0` when it ran already (skip the call).
    pub claim: unsafe fn(*mut std::ffi::c_void, u32, u64) -> u64,
    /// Resume the generator word (with the sent word when the `bool` says
    /// so), as `ResumeCoro` does: its body runs on the VM until it yields
    /// or returns, and that word comes back; same panic rules as `call`.
    pub resume: unsafe fn(*mut std::ffi::c_void, *mut NativeCtx, u64, Option<u64>, &mut bool) -> u64,
    /// Collect, with the context's roots pinned.
    pub collect: unsafe fn(*mut std::ffi::c_void, *mut NativeCtx),
    /// `gc_collect`: a full collection, with the context's roots pinned;
    /// returns the bytes freed and sets the `bool` when a `drop` panicked.
    pub gc: unsafe fn(*mut std::ffi::c_void, *mut NativeCtx, &mut bool) -> u64,
    /// `FreeEarly` now, running the `drop`s it owes (with the context's
    /// roots pinned); `false` when one panicked (the VM reported it).
    pub free: unsafe fn(*mut std::ffi::c_void, *mut NativeCtx, u64, bool) -> bool,
    /// Run the host native `id` that needs the VM on `args` in `layout`
    /// through its stub ([`host_stubs`]), as `call` runs a body; `None`
    /// when the machine has no stub for it (nothing ran).
    pub host: HostViaVmFn,
    /// Run the FFI op (`FfiLoad` / `DeclareFFI` / `FfiInvoke`, by opcode
    /// byte) with its operand on `args`, on the runtime.
    pub ffi: HostViaVmFn,
    /// Run the ordinary host native `id` on `args` in `layout` as the VM's
    /// `HostInvoke` does, without bytecode: a native that would block the
    /// task parks the fiber the call runs on. `None`: it is no ordinary
    /// native (nothing ran).
    pub invoke: InvokeFn,
}

/// [`NativeHost::invoke`]: machine, context, native id, layout, arguments.
pub type InvokeFn =
    unsafe fn(*mut std::ffi::c_void, *mut NativeCtx, u64, u64, &[common::Value]) -> Option<Invoked>;

/// What [`NativeHost::invoke`] did.
pub enum Invoked {
    /// It returned this word.
    Value(u64),
    /// It panicked with this message (not reported yet).
    Panic(String),
    /// The task was cancelled while it waited: the VM armed the unwind.
    Unwound,
}


/// [`NativeHost::host`]: machine, context, native id, layout, arguments,
/// panicked flag.
pub type HostViaVmFn = unsafe fn(*mut std::ffi::c_void, *mut NativeCtx, u64, u64, &[common::Value], &mut bool) -> Option<u64>;

impl NativeHost {
    const NONE: Self = Self {
        print: |_, _| {},
        call: |_, _, _, _, _| 0,
        call_value: |_, _, _, _, _, _| 0,
        resume: |_, _, _, _, _| 0,
        claim: |_, _, _| 1,
        collect: |_, _| {},
        gc: |_, _, _| 0,
        free: |_, _, _, _| true,
        host: |_, _, _, _, _, _| None,
        ffi: |_, _, _, _, _, _| None,
        invoke: |_, _, _, _, _| None,
    };
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
/// Not a panic: the call's fiber parked (its task suspended inside it);
/// the task suspends at the `CALL` (`vm_fiber.rs`).
pub const TRAP_PARKED: u64 = 10;
/// Not a panic: a generator's close unwinds its body from the yield it is
/// suspended at, collecting its armed `defer`s (`vm_gen.rs`).
pub const TRAP_CLOSE: u64 = 11;
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

/// Byte offset of [`NativeCtx::bail`].
pub const NATIVE_CTX_BAIL: i32 = 0;
/// Byte offset of [`NativeCtx::stack_limit`].
pub const NATIVE_CTX_STACK_LIMIT: i32 = 8;

impl NativeCtx {
    /// A context for one call from the VM.
    pub fn new(stack_limit: u64) -> Self {
        Self {
            bail: 0,
            stack_limit,
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
            pads: std::ptr::null_mut(),
            calls: 0,
        }
    }

    /// As [`Self::new`], with `heap` for the helpers that allocate.
    pub fn with_heap(stack_limit: u64, heap: *mut crate::memory::Heap) -> Self {
        Self { heap, ..Self::new(stack_limit) }
    }
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
    /// It may run VM code (a VM call, a function value, a `drop`), so a
    /// task switch could come while its frames are live: with a scheduler
    /// that can switch, the call runs on the task's own fiber stack
    /// (`vm_fiber.rs`).
    pub suspends: bool,
    /// The leading parameters the body reads: a `CALL` of at least this
    /// many words (and at most `arity`) enters it, the rest passed as `0`.
    pub reads: u16,
    /// Result words: `1`, or `2` for a two-slot return (`lo`, then `hi`
    /// in [`NativeCtx::ret_hi`]), as the VM's `RETURN` pushes them.
    pub ret_words: u8,
    /// A generator's body: only a resume enters it, on a fiber of its own
    /// (`native_gen.rs`), never a `CALL`.
    pub generator: bool,
    /// It has `defer`s (a cleanup pad): a generator's close resumes its
    /// fiber to unwind them (`vm_gen.rs`).
    pub cleanup: bool,
    pub entry: NativeEntryFn,
}

/// A [`NativeEntry`] as an AOT object's `coil_aot_entries` table lays it
/// out (`coil_native::aot` writes it, [`install_aot`] reads it).
#[repr(C)]
pub struct AotEntry {
    pub pc: u32,
    pub arity: u16,
    pub reads: u16,
    pub ret_words: u8,
    /// `1` when it may run VM code ([`NativeEntry::suspends`]).
    pub suspends: u8,
    /// `1` for a generator's body ([`NativeEntry::generator`]).
    pub generator: u8,
    /// `1` when it has `defer`s ([`NativeEntry::cleanup`]).
    pub cleanup: u8,
    pub pad: [u8; 4],
    pub entry: NativeEntryFn,
}

/// Install the code an AOT object linked into this executable defines,
/// for the next run: `n` entries and `n_sites` sites, each four words
/// (`entry_pc`, then the location's file, start and end byte).
///
/// # Safety
///
/// The tables are the linked object's, as `coil_native::aot` wrote them.
pub unsafe fn install_aot(entries: *const AotEntry, n: usize, sites: *const u32, n_sites: usize) {
    // SAFETY: the object defines `n` entries and `4 * n_sites` words.
    let entries = unsafe { std::slice::from_raw_parts(entries, n) };
    let words = unsafe { std::slice::from_raw_parts(sites, n_sites * 4) };
    let entries = entries
        .iter()
        .map(|e| NativeEntry {
            pc: e.pc,
            arity: e.arity,
            suspends: e.suspends != 0,
            reads: e.reads,
            ret_words: e.ret_words,
            generator: e.generator != 0,
            cleanup: e.cleanup != 0,
            entry: e.entry,
        })
        .collect();
    let sites = words
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| NativeSite { entry_pc: w[0], loc: common::DebugLoc { file: w[1], start_byte: w[2], end_byte: w[3] } })
        .collect();
    install_for_next_run(entries, sites);
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

    /// The program's length, where the host-call stubs start.
    pub(crate) fn code_len(&self) -> usize {
        self.by_pc.len()
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
    /// Bytecode bodies the VM ran (`CALL`s native code did not take).
    pub(crate) vm_runs: u64,
    /// Operand stack base of the last call that bailed: calls whose base
    /// is above it run in the VM (`usize::MAX`: none).
    pub(crate) quiet_above: usize,
}

impl NativeTable {
    pub(crate) fn new(code: std::sync::Arc<NativeCode>) -> Self {
        Self {
            code,
            hits: 0,
            bails: 0,
            vm_runs: 0,
            quiet_above: usize::MAX,
        }
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
    /// The unwound frames' cleanup pads, innermost first.
    pub(crate) pads: Vec<NativePad>,
}

impl NativePanic {
    /// A fiber parked ([`TRAP_PARKED`]).
    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    pub(crate) fn parked() -> Self {
        Self { code: TRAP_PARKED, trace: Vec::new(), message: String::new(), pads: Vec::new() }
    }

    /// The VM's message for the same trap.
    pub(crate) fn message(&self) -> &str {
        match self.code {
            TRAP_MESSAGE | TRAP_EXPLICIT => &self.message,
            TRAP_OVERFLOW => common::int_arith::IntTrap::Overflow.message(),
            TRAP_DIV_ZERO => common::int_arith::IntTrap::DivByZero.message(),
            TRAP_NEGATIVE_EXPONENT => common::int_arith::IntTrap::NegativeExponent.message(),
            TRAP_SHIFT => common::int_arith::IntTrap::ShiftOutOfRange.message(),
            TRAP_OUT_OF_BOUNDS => "index out of bounds",
            TRAP_STACK => crate::stacks::STACK_OVERFLOW,
            _ => "native code reached an operation it does not model",
        }
    }
}

/// How a native call ended.
pub(crate) enum NativeOutcome {
    Done(NativeResult),
    /// The call never started (no fiber stack): the VM runs it. Native
    /// code commits at entry, so nothing else bails.
    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    Bailed,
    Panicked(NativePanic),
}

/// Run `entry` with `args`.
#[inline(never)]
pub(crate) fn call(entry: &NativeEntry, args: &[u64], env: &NativeEnv) -> NativeOutcome {
    let marker = 0u8;
    let sp = std::ptr::addr_of!(marker) as u64;
    let mut ctx = NativeCtx::with_heap(sp.saturating_sub(NATIVE_STACK_BUDGET), env.heap);
    if let Some(limit) = crate::stacks::fiber_stack_limit() {
        ctx.stack_hard = ctx.stack_hard.min(limit as u64);
    } else if let Some(left) = stacker::remaining_stack() {
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
    let mut pads = Vec::new();
    ctx.pads = &mut pads;
    // One mode: the call is committed from its first instruction. A trap
    // panics natively with the VM's message, the stack runs to the
    // thread's real end, and collections run at safepoints; nothing hands
    // the call back to the VM.
    ctx.committed = 1;
    ctx.stack_limit = ctx.stack_hard;
    // SAFETY: the backend compiled `entry` for exactly `arity` argument
    // words, which `args` holds; `ctx` outlives the call.
    let out = unsafe { (entry.entry)(&mut ctx, args.as_ptr()) };
    match ctx.bail {
        0 => {
            let hi = (entry.ret_words == 2).then_some(ctx.ret_hi);
            NativeOutcome::Done(NativeResult { lo: out, hi, allocated: ctx.allocs != 0 || ctx.vm_calls != 0, frees })
        }
        // Its effects stay, as a VM panic's do. A committed call cannot
        // bail, so any other flag is a panic as well.
        _ => NativeOutcome::Panicked(NativePanic { code: ctx.trap, trace, message, pads }),
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_unwind(ctx: *mut NativeCtx, code: u64, site: u64) {
    let ctx = unsafe { &mut *ctx };
    if ctx.bail != BAIL_PANIC {
        if ctx.committed == 0 {
            // A panic the VM's re-run would raise too panics here: native
            // code reports it as the VM does. Anything else (an op it does
            // not model, the native stack) hands the call back to the VM.
            let trap = if code == TRAP_HELPER { ctx.trap } else { code };
            if !panics_natively(trap) {
                if let Some(n) = BAIL_TRAPS.get(trap as usize) {
                    n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                ctx.bail = 1;
                return;
            }
            ctx.committed = 1;
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

/// A trap code that is a Coil panic wherever it comes from: before a
/// commit, native code raises it itself instead of bailing.
fn panics_natively(trap: u64) -> bool {
    matches!(
        trap,
        TRAP_OVERFLOW
            | TRAP_DIV_ZERO
            | TRAP_NEGATIVE_EXPONENT
            | TRAP_SHIFT
            | TRAP_OUT_OF_BOUNDS
            | TRAP_MESSAGE
            | TRAP_EXPLICIT
    )
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
#[unsafe(no_mangle)]
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
#[unsafe(no_mangle)]
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
#[unsafe(no_mangle)]
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_store_index(ctx: *mut NativeCtx, addr: u64, index: i64, value: u64) -> u64 {
    let Some(crate::memory::Object::Array(mut gc)) = object(addr) else {
        return unsafe { bail(ctx) };
    };
    if gc.as_ref().read_indexed(index, false).is_none() {
        unsafe { (*ctx).trap = TRAP_OUT_OF_BOUNDS };
        return unsafe { bail(ctx) };
    }
    gc.as_mut().store_indexed(index, common::Value::from(value), false);
    value
}

/// `SetField` of slot `index` on an instance with slots; return `value`.
/// Anything else: bail.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `addr` is a heap word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_set_field(ctx: *mut NativeCtx, addr: u64, index: u64, value: u64) -> u64 {
    let slot = index as usize;
    let Some(crate::memory::Object::Instance(mut gc)) = object(addr) else {
        return unsafe { bail(ctx) };
    };
    if gc.as_ref().slot_len().is_none_or(|n| slot >= n) {
        return unsafe { bail(ctx) };
    }
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_view(addr: u64, for_store: u64, out: *mut u64) -> u64 {
    let Some(crate::memory::Object::Array(mut gc)) = object(addr) else {
        return 0;
    };
    let view = if for_store != 0 { gc.as_mut().words_view(true) } else { gc.as_ref().words_view_for_load() };
    match view {
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

/// Count one allocation; past [`NATIVE_ALLOC_CAP`] with a collection
/// due, commit and collect at the next safepoint.
unsafe fn note_alloc(ctx: *mut NativeCtx, heap: &crate::memory::Heap) {
    let ctx = unsafe { &mut *ctx };
    ctx.allocs += 1;
    if ctx.committed != 0 {
        // The call cannot bail any more: it collects at a safepoint.
        if !heap.gc_is_idle() || heap.should_collect() {
            ctx.gc_due = 1;
        }
    } else if ctx.allocs >= NATIVE_ALLOC_CAP && heap.should_collect() {
        // Commit rather than hand the call back to the VM: it collects at
        // its next safepoint, and a later trap panics natively.
        ctx.committed = 1;
        ctx.gc_due = 1;
    }
}

/// A safepoint found [`NativeCtx::gc_due`]: native code linked its heap
/// words into [`NativeCtx::roots`]; the VM collects with them pinned.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`].
#[unsafe(no_mangle)]
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_cow(ctx: *mut NativeCtx, word: u64, unique: u64) -> u64 {
    if crate::runtime::cow_eager() {
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
/// writing the slot back only when it copied.
/// Returns `obj`.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `obj` is a heap word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_cow_field(ctx: *mut NativeCtx, obj: u64, field: u64) -> u64 {
    if crate::runtime::cow_eager() {
        return unsafe { bail(ctx) };
    }
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    if let Some(crate::memory::Object::Instance(mut gc)) = object(obj)
        && let Some(v) = gc.as_ref().slot(field as usize)
        && let Some(copy) = heap.cow_unique(v)
    {
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
pub const ALLOC_FN: u64 = 4;
pub const ALLOC_POLY_FN: u64 = 5;
pub const ALLOC_DROP_ENUM: u64 = 6;
pub const ALLOC_CORO: u64 = 7;

/// A new object of `kind` from the `n` words at `words`, in declaration
/// order (an enum's payload order):
/// - [`ALLOC_ARRAY`]: `extra` is the element word kind (`0`: none);
/// - [`ALLOC_TUPLE`]: `extra` is the packed word kinds;
/// - [`ALLOC_ENUM`]: `extra` is `tag | kinds << 32`;
/// - [`ALLOC_OBJECT`]: `extra` is the type id and `n` the field count
///   (`words` unused: stores fill the fields);
/// - [`ALLOC_FN`]: a function value, as `MakeFn`: `extra` is the entry PC
///   `| operand << 32`, the words its captures and filled parameters, then
///   the filled mask;
/// - [`ALLOC_POLY_FN`]: a generic function's value, as `MakePolyFn` /
///   `MakePolyFnCapture`: `extra` is the entry PC `| params << 32`, the
///   words its dictionaries (`0`: filled in by the call);
/// - [`ALLOC_DROP_ENUM`]: [`ALLOC_ENUM`] of an enum with a `drop`, as
///   `TagEnumType` stamps it: the last word is its type id;
/// - [`ALLOC_CORO`]: a generator, as `MakeCoro`: `extra` is its body's
///   entry PC, the words its arguments.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `words` points at `n` words.
#[unsafe(no_mangle)]
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
                // Not allocated yet: the allocation counts the packed size.
                let _ = arr.take_resize();
            }
            heap.alloc(arr, Object::Array).0.addr()
        }
        ALLOC_TUPLE if n == 0 => return heap.immortal_empty_tuple().addr(),
        ALLOC_TUPLE => heap.alloc(ObjTuple::from_slice(&values).with_kinds(extra as u8), Object::Tuple).0.addr(),
        ALLOC_DROP_ENUM => {
            let Some((&type_id, payload)) = values.split_last() else {
                return unsafe { bail(ctx) };
            };
            if payload.is_empty() {
                // A unit variant is a shared immortal, untagged.
                return heap.immortal_unit_enum(extra as u32).addr();
            }
            let payload = match payload {
                [a] => EnumPayload::one(*a),
                [a, b] => EnumPayload::two(*a, *b),
                v => EnumPayload::from_slice(v),
            }
            .with_kinds((extra >> 32) as u8);
            let (obj, mut gc) = heap.alloc(ObjEnum::new(extra as u32, payload), Object::Enum);
            let type_id = type_id.bits() as u32;
            gc.as_mut().type_id = type_id;
            heap.note_enum_type(Object::Enum(gc), type_id);
            obj.addr()
        }
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
            let (_, r) = heap.alloc(ObjInstance::with_type_id_and_fields(extra as u32, n), Object::Instance);
            r.as_ptr().addr() as u64
        }
        ALLOC_CORO => {
            let mut live_mask = 0u64;
            for (i, v) in values.iter().take(64).enumerate() {
                let a = v.heap_addr();
                if a != 0 && heap.find_object_by_addr(a).is_some() {
                    live_mask |= 1 << i;
                }
            }
            let target = extra as usize;
            let coro = crate::memory::ObjCoroutine {
                state: crate::CoroState::Suspended,
                resume_ip: target,
                saved_stack: values,
                saved_live_mask: live_mask,
                saved_frames: vec![(target, 0)],
                pending_send: common::Value::from(0_i64),
                yield_from: None,
                delegator: None,
                yield_from_resume_ip: 0,
                started: false,
                closeable: false,
                native: None,
            };
            heap.alloc(coro, Object::Coroutine).0.addr()
        }
        ALLOC_FN => {
            let op = (extra >> 32) as u32;
            let (n_captures, n_filled) = ((op & 0xFF) as usize, ((op >> 8) & 0xFF) as usize);
            let Some((&mask, rest)) = values.split_last() else {
                return unsafe { bail(ctx) };
            };
            if rest.len() != n_captures + n_filled {
                return unsafe { bail(ctx) };
            }
            let (captures, filled) = rest.split_at(n_captures);
            let pfn = crate::memory::ObjFn {
                entry: extra as u32,
                arity: (op >> 16) & 0xFF,
                is_rest: op & (1 << 24) != 0,
                // `Const mask` then `as_int() as u64`, as `MakeFn` reads it.
                filled_mask: i64::from(mask.bits() as u32 as i32) as u64,
                captured_args: filled.to_vec(),
                captures: captures.to_vec(),
                dicts: None,
            };
            heap.alloc(pfn, Object::Fn).0.addr()
        }
        ALLOC_POLY_FN => {
            use crate::memory::Member;
            let dicts = values
                .iter()
                .map(|&v| match v.bits() {
                    0 => None,
                    addr => Some(heap.find_object_by_addr(addr).map_or(Member::Value(v), Member::Object)),
                })
                .collect();
            heap.alloc(crate::memory::ObjFn::poly(extra as u32, (extra >> 32) as u32, dicts), Object::Fn).0.addr()
        }
        _ => return unsafe { bail(ctx) },
    };
    unsafe { note_alloc(ctx, heap) };
    addr
}

/// `ArrayPush`: append `value` to the array `addr` and return `addr`. Not an array: bail, where the VM panics.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `addr` is a heap word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_array_push(ctx: *mut NativeCtx, addr: u64, value: u64) -> u64 {
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    let Some(crate::memory::Object::Array(mut gc)) = object(addr) else {
        return unsafe { bail(ctx) };
    };
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
#[unsafe(no_mangle)]
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
#[unsafe(no_mangle)]
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
#[unsafe(no_mangle)]
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
#[unsafe(no_mangle)]
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
#[unsafe(no_mangle)]
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
#[unsafe(no_mangle)]
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
        let message = crate::format::render_format(rest, || words.next().unwrap_or_default());
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
/// the VM frees it now. With finalizers in the program the free may run
/// `drop`s, which cannot be taken back: the call commits, and the VM frees
/// it now as its `FreeEarly` does (a collection may run: native code roots
/// its words around this call then). A `drop` that panicked unwinds with
/// [`TRAP_VM_PANIC`].
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_free_early(ctx: *mut NativeCtx, word: u64, deep: u64) -> u64 {
    let c = unsafe { &mut *ctx };
    if c.frees.is_null() {
        return unsafe { bail(ctx) };
    }
    if c.has_finalizers != 0 {
        if c.machine.is_null() {
            return unsafe { bail(ctx) };
        }
        c.committed = 1;
        c.vm_calls += 1;
        c.gc_epoch += 1;
        let (machine, free) = (c.machine, c.host.free);
        // SAFETY: `machine` is the live machine `host` was made for.
        if !unsafe { free(machine, ctx, word, deep != 0) } {
            let c = unsafe { &mut *ctx };
            c.trap = TRAP_VM_PANIC;
            return unsafe { bail(ctx) };
        }
        return 0;
    }
    if c.committed != 0 && !c.machine.is_null() {
        // SAFETY: `machine` is the live machine `host` was made for.
        let _ = unsafe { (c.host.free)(c.machine, ctx, word, deep != 0) };
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
#[unsafe(no_mangle)]
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
#[unsafe(no_mangle)]
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

/// `CallIndirect` from native code: call the function value (or code
/// pointer) `callee` on the `n` words at `args`, the last `dicts` of them
/// instance dictionaries, on the VM. Commits first, as
/// [`coil_native_call_vm`] does, with the same panic rule.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `args` points at `n` words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_call_value(ctx: *mut NativeCtx, callee: u64, args: *const u64, n: u64, dicts: u64) -> u64 {
    let c = unsafe { &mut *ctx };
    let mut buf = [common::Value::default(); 16];
    let Some(words) = buf.get_mut(..n as usize) else {
        return unsafe { bail(ctx) };
    };
    if c.machine.is_null() || dicts > n {
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
    let (machine, call) = (c.machine, c.host.call_value);
    // SAFETY: `machine` is the live machine `host` was made for.
    let out = unsafe { call(machine, ctx, callee, words, dicts as u32, &mut panicked) };
    let c = unsafe { &mut *ctx };
    if panicked {
        c.trap = TRAP_VM_PANIC;
        return unsafe { bail(ctx) };
    }
    out
}

/// `ResumeCoro`: resume a generator ([`NativeHost::resume`]).
///
/// # Safety
///
/// `ctx` is the calling native code's live context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_resume(ctx: *mut NativeCtx, handle: u64, send: u64, has_send: u64) -> u64 {
    let c = unsafe { &mut *ctx };
    if c.machine.is_null() {
        return unsafe { bail(ctx) };
    }
    // A resume that cannot run panics here, at native code's own site.
    // SAFETY: the context's heap is the machine's.
    let refused = match unsafe { &*c.heap }.find_object_by_addr(handle) {
        Some(crate::Object::Coroutine(gc)) if gc.as_ref().state == crate::CoroState::Done => Some("resumed after completion"),
        Some(crate::Object::Coroutine(_)) => None,
        _ => Some("resumed invalid coroutine handle"),
    };
    if let Some(msg) = refused {
        c.trap = TRAP_MESSAGE;
        // SAFETY: `call` set `message` for the call.
        unsafe { *c.message = msg.to_string() };
        return unsafe { bail(ctx) };
    }
    c.committed = 1;
    c.vm_calls += 1;
    c.gc_epoch += 1;
    let mut panicked = false;
    let (machine, resume) = (c.machine, c.host.resume);
    let send = (has_send != 0).then_some(send);
    // SAFETY: `machine` is the live machine `host` was made for.
    let out = unsafe { resume(machine, ctx, handle, send, &mut panicked) };
    let c = unsafe { &mut *ctx };
    if panicked {
        c.trap = TRAP_VM_PANIC;
        return unsafe { bail(ctx) };
    }
    out
}

/// `DoneCoro`: `1` when the generator word finished (a word that is no
/// generator is not).
///
/// # Safety
///
/// `ctx` is a live context with its heap.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_coro_done(ctx: *mut NativeCtx, handle: u64) -> u64 {
    // SAFETY: the context's heap is the machine's.
    let heap = unsafe { &*(*ctx).heap };
    u64::from(matches!(
        heap.find_object_by_addr(handle),
        Some(crate::Object::Coroutine(gc)) if gc.as_ref().state == crate::CoroState::Done
    ))
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
                let collect = n.name() == crate::gc_handles::GC_COLLECT_NATIVE;
                let via_vm = func.is_none() && !collect && matches!(n.host_op(), crate::HostOp::Ordinary | crate::HostOp::Task | crate::HostOp::RegionEnter | crate::HostOp::RegionExit | crate::HostOp::RegisterFinalizer);
                DirectHost { name: n.name().to_string(), func, guarded, collect, via_vm }
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
    /// `gc_collect`: a full collection on the VM ([`NativeHost::gc`]),
    /// with native code's words rooted around the call.
    pub collect: bool,
    /// An ordinary native that needs the VM (it may park the task, open
    /// descriptors, ...), a task op, a `region` hook (a minor collection)
    /// or the finalizer registry's `gc_register_finalizer`:
    /// native code runs it through a host-call stub
    /// ([`host_stubs`]) in a nested VM execution, as a VM call.
    pub via_vm: bool,
}

/// Arities a host-call stub exists for (the words after the native id).
pub(crate) const HOST_STUB_ARITIES: usize = 16;
/// Enum layouts a host-call stub exists for (`common::HOST_ENUM_LAYOUT_*`
/// below the reserved one).
const HOST_STUB_LAYOUTS: usize = 3;
/// Code words the stubs take past the program.
pub(crate) const HOST_STUBS_LEN: usize = 2 * HOST_STUB_ARITIES * HOST_STUB_LAYOUTS;

/// The host-call stubs a machine with native code appends to its program:
/// for each layout and arity, `HostInvoke; RETURN`. Called with the native
/// id and the arguments as its frame, a stub runs the host call as a VM
/// body would: parking, retries and panics as in the VM.
pub(crate) fn host_stubs() -> Vec<common::Byte> {
    let mut out = Vec::with_capacity(HOST_STUBS_LEN);
    for layout in 0..HOST_STUB_LAYOUTS as u32 {
        for arity in 0..HOST_STUB_ARITIES as u32 {
            out.push(common::Byte::new(common::Instruction::HostInvoke).with_operand_u32(common::pack_host_invoke_operand(arity, layout)));
            out.push(common::Byte::new(common::Instruction::RETURN));
        }
    }
    out
}

/// Where the stub for `arity` words in `layout` starts, past the program.
pub(crate) fn host_stub_offset(arity: usize, layout: u64) -> Option<usize> {
    (arity < HOST_STUB_ARITIES && (layout as usize) < HOST_STUB_LAYOUTS).then(|| 2 * (layout as usize * HOST_STUB_ARITIES + arity))
}

/// `HostInvoke` of the direct host native `id` ([`direct_host_natives`])
/// on the `n` words at `args`, constructing enums in `layout`
/// (`common::HOST_ENUM_LAYOUT_*`), as the VM's direct path does. The
/// native's effects cannot be taken back: the call commits first.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `args` points at `n` words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_host(ctx: *mut NativeCtx, id: u64, layout: u64, args: *const u64, n: u64) -> u64 {
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    let c = unsafe { &mut *ctx };
    if direct_host_natives().get(id as usize).is_some_and(|h| h.collect) {
        if c.machine.is_null() {
            return unsafe { bail(ctx) };
        }
        // Finalizers it runs cannot be taken back.
        c.committed = 1;
        c.vm_calls += 1;
        c.gc_epoch += 1;
        let mut panicked = false;
        // SAFETY: `machine` is the live machine `host` was made for.
        let freed = unsafe { (c.host.gc)(c.machine, ctx, &mut panicked) };
        if panicked {
            let c = unsafe { &mut *ctx };
            c.trap = TRAP_VM_PANIC;
            return unsafe { bail(ctx) };
        }
        return freed;
    }
    if direct_host_natives()
        .get(id as usize)
        .is_some_and(|h| h.via_vm)
    {
        if let Some(out) = unsafe { host_invoke(ctx, id, layout, args, n) } {
            return out;
        }
        let host = unsafe { (*ctx).host.host };
        note_host_call(id as usize, HostPath::Stub);
        return unsafe { host_via_vm(ctx, host, id, layout, args, n) };
    }
    let Some(DirectHost { name, func: Some(f), guarded, .. }) = direct_host_natives().get(id as usize) else {
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

/// `Yield` in a generator's body: suspend its fiber with `value` (the
/// resumer takes the frame's root words into the generator meanwhile and
/// puts them back, forwarded, before it resumes); the word the resumer
/// sends comes back. What the resumer sees cannot be taken back: commit.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`], on a generator's fiber.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_yield(ctx: *mut NativeCtx, value: u64) -> u64 {
    unsafe { (*ctx).committed = 1 };
    let send = gen_suspend(ctx, value);
    let c = unsafe { &mut *ctx };
    // The words may have moved while it was away.
    c.gc_epoch += 1;
    if GEN_CLOSING.replace(false) {
        // Closed while suspended: unwind from here, as a panic does.
        c.trap = TRAP_CLOSE;
        c.bail = BAIL_PANIC;
        return 0;
    }
    send
}

thread_local! {
    /// The next generator resume is a close ([`coil_native_yield`]).
    pub(crate) static GEN_CLOSING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(not(target_family = "wasm"))]
fn gen_suspend(ctx: *mut NativeCtx, value: u64) -> u64 {
    let y = GEN_YIELDER.get().cast::<corosensei::Yielder<u64, (u64, usize)>>();
    assert!(!y.is_null(), "a yield outside a generator's fiber");
    // SAFETY: the running generator fiber set its yielder.
    unsafe { (*y).suspend((value, ctx as usize)) }
}

#[cfg(target_family = "wasm")]
fn gen_suspend(_ctx: *mut NativeCtx, _value: u64) -> u64 {
    unreachable!("no generator fibers on wasm")
}

#[cfg(not(target_family = "wasm"))]
thread_local! {
    /// The running generator fiber's yielder.
    pub(crate) static GEN_YIELDER: std::cell::Cell<*const ()> = const { std::cell::Cell::new(std::ptr::null()) };
}

/// A generator's fiber, for a generator whose body is native code: resumes
/// send it a word, a yield gives back the yielded word and the parked
/// call's context, and it returns the body's outcome.
#[cfg(not(target_family = "wasm"))]
pub(crate) type GenCoro = corosensei::Coroutine<u64, (u64, usize), NativeOutcome, corosensei::stack::DefaultStack>;

/// The native side of a generator whose body runs natively: owned by its
/// `ObjCoroutine` while it is suspended (`vm_gen.rs`).
#[cfg_attr(target_family = "wasm", allow(dead_code))]
pub struct GenFiber {
    #[cfg(not(target_family = "wasm"))]
    pub(crate) coro: Option<GenCoro>,
    pub(crate) stack_limit: usize,
    pub(crate) yielder: *const (),
    /// The context of the call suspended at a yield (its root words are
    /// the generator's saved words meanwhile).
    pub(crate) parked: *mut NativeCtx,
    /// Its body has `defer`s: a close resumes it to unwind them.
    pub(crate) cleanup: bool,
}

impl Drop for GenFiber {
    fn drop(&mut self) {
        #[cfg(not(target_family = "wasm"))]
        if let Some(coro) = &mut self.coro
            && coro.started()
            && !coro.done()
        {
            // A generator dropped while suspended: its native frames cannot
            // unwind (no unwind tables), so the stack is reclaimed as it is.
            // SAFETY: nothing runs on the fiber's stack again.
            unsafe { coro.force_reset() };
        }
    }
}

/// An FFI op from native code (`MirInst::Ffi`): opcode byte `op` with
/// `operand` on the `n` words at `args`, on the runtime
/// ([`NativeHost::ffi`]), with the commit and panic rules of a host call.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `args` points at `n` words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_ffi(ctx: *mut NativeCtx, op: u64, operand: u64, args: *const u64, n: u64) -> u64 {
    let ffi = unsafe { (*ctx).host.ffi };
    let kind = [common::Instruction::FfiLoad as u8, common::Instruction::DeclareFFI as u8].iter().position(|o| u64::from(*o) == op).unwrap_or(2);
    FFI_CALLS[kind].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    unsafe { host_on_machine(ctx, ffi, op, operand, args, n, false) }
}

/// The opcodes of FFI ops, which get a stub per operand the program uses.
pub(crate) fn is_ffi_op(op: common::Instruction) -> bool {
    matches!(op, common::Instruction::FfiLoad | common::Instruction::DeclareFFI | common::Instruction::FfiInvoke)
}

/// [`coil_native_host`] of an ordinary native that is no plain function:
/// the machine runs it in Rust ([`NativeHost::invoke`]), no bytecode. Its
/// effects cannot be taken back: the call commits first. `None`: it is
/// no ordinary native, and its stub runs it.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `args` points at `n` words.
unsafe fn host_invoke(
    ctx: *mut NativeCtx,
    id: u64,
    layout: u64,
    args: *const u64,
    n: u64,
) -> Option<u64> {
    let c = unsafe { &mut *ctx };
    let mut buf = [common::Value::default(); HOST_STUB_ARITIES];
    let words = buf.get_mut(..n as usize)?;
    if c.machine.is_null() || c.heap.is_null() {
        return None;
    }
    for (i, w) in words.iter_mut().enumerate() {
        // SAFETY: `args` holds `n` words.
        *w = common::Value::from(unsafe { *args.add(i) });
    }
    // SAFETY: the context's heap is the machine's.
    let live_before = unsafe { &*c.heap }.live_object_count();
    let committed = std::mem::replace(&mut c.committed, 1);
    // A wait lets other tasks run, and they may collect.
    c.gc_epoch += 1;
    let (machine, invoke) = (c.machine, c.host.invoke);
    // SAFETY: `machine` is the live machine `host` was made for.
    let out = unsafe { invoke(machine, ctx, id, layout, words) };
    let c = unsafe { &mut *ctx };
    match out {
        None => {
            c.committed = committed;
            None
        }
        Some(Invoked::Value(v)) => {
            // SAFETY: as above.
            let heap = unsafe { &*c.heap };
            let made = heap.live_object_count().saturating_sub(live_before) as u64;
            if made != 0 {
                c.allocs += made - 1;
                unsafe { note_alloc(ctx, heap) };
            }
            Some(v)
        }
        Some(Invoked::Panic(msg)) => {
            c.trap = TRAP_MESSAGE;
            // SAFETY: `call` set `message` for the call.
            unsafe { *c.message = msg };
            Some(unsafe { bail(ctx) })
        }
        Some(Invoked::Unwound) => {
            c.trap = TRAP_VM_PANIC;
            Some(unsafe { bail(ctx) })
        }
    }
}

/// [`coil_native_host`] of a native that needs the VM: its stub runs in a
/// nested VM execution ([`NativeHost::host`]), as [`coil_native_call_vm`]
/// runs a body, with the same commit and panic rules.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `args` points at `n` words.
unsafe fn host_via_vm(ctx: *mut NativeCtx, host: HostViaVmFn, id: u64, layout: u64, args: *const u64, n: u64) -> u64 {
    unsafe { host_on_machine(ctx, host, id, layout, args, n, true) }
}

/// [`host_via_vm`], or (`!via_vm`) a host entry that runs in Rust and
/// enters no VM stub.
///
/// # Safety
/// As [`host_via_vm`].
unsafe fn host_on_machine(ctx: *mut NativeCtx, host: HostViaVmFn, id: u64, layout: u64, args: *const u64, n: u64, via_vm: bool) -> u64 {
    let c = unsafe { &mut *ctx };
    let mut buf = [common::Value::default(); HOST_STUB_ARITIES];
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
    let committed = std::mem::replace(&mut c.committed, 1);
    // It may allocate (the call reports `allocated`).
    c.vm_calls += 1;
    if via_vm {
        HOST_VIA_VM.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    c.gc_epoch += 1;
    let mut panicked = false;
    let machine = c.machine;
    // SAFETY: `machine` is the live machine `host` was made for.
    let out = unsafe { host(machine, ctx, id, layout, words, &mut panicked) };
    let c = unsafe { &mut *ctx };
    let Some(out) = out else {
        // No stub: nothing ran.
        c.committed = committed;
        return unsafe { bail(ctx) };
    };
    if panicked {
        c.trap = TRAP_VM_PANIC;
        return unsafe { bail(ctx) };
    }
    out
}

/// A frame with `defer`s on its way out: after a committed panic, keep its
/// cleanup pad `func` and a copy of its `n` homes at `homes` (`mask`: the
/// heap words among them) for the VM to run ([`NativeCtx::pads`]). A
/// bail before the commit runs nothing: the VM runs the call again.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `homes` points at `n` words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_defer(ctx: *mut NativeCtx, func: u64, homes: *const u64, n: u64, mask: u64) {
    let c = unsafe { &mut *ctx };
    if c.bail != BAIL_PANIC || c.pads.is_null() || func == 0 {
        return;
    }
    // `n` may carry `ROOTS_AMBIGUOUS` (an opaque body's `int` homes).
    let (flags, n) = (n & ROOTS_AMBIGUOUS, (n & !ROOTS_AMBIGUOUS).min(64));
    let mut rec = vec![0, n | ROOTS_MASKED | flags, mask];
    // SAFETY: `homes` holds `n` words.
    rec.extend((0..n as usize).map(|i| unsafe { *homes.add(i) }));
    // SAFETY: the backend passes a function with the `NativePadFn` ABI.
    let func = unsafe { std::mem::transmute::<usize, NativePadFn>(func as usize) };
    unsafe { (*c.pads).push(NativePad { func, homes: rec }) };
}

/// Run the cleanup pads of a native panic, innermost first, on `env`'s
/// machine (committed: whatever they do is for good). Each pad's homes,
/// and those of the pads still to run, are roots meanwhile.
pub(crate) fn run_pads(
    mut pads: Vec<NativePad>,
    env: &NativeEnv,
    on_panic: &mut dyn FnMut(NativePanic),
) {
    let marker = 0u8;
    let sp = std::ptr::addr_of!(marker) as u64;
    // Link every record: the chain is innermost-last so each pad's own
    // homes stay listed while it runs.
    for i in 1..pads.len() {
        let next = pads[i - 1].homes.as_mut_ptr() as u64;
        pads[i].homes[0] = next;
    }
    for i in 0..pads.len() {
        let mut ctx = NativeCtx::with_heap(sp.saturating_sub(NATIVE_STACK_BUDGET), env.heap);
        if let Some(limit) = crate::stacks::fiber_stack_limit() {
            ctx.stack_hard = ctx.stack_hard.min(limit as u64);
        } else if let Some(left) = stacker::remaining_stack() {
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
        ctx.committed = 1;
        // The last record links all before it.
        let last = pads.len() - 1;
        ctx.roots = pads[last].homes.as_mut_ptr() as u64;
        let mut inner = Vec::new();
        ctx.pads = &mut inner;
        let homes = pads[i].homes[3..].as_ptr();
        // SAFETY: the backend compiled `func` as this frame's pad.
        let _ = unsafe { (pads[i].func)(&mut ctx, homes) };
        if ctx.bail == BAIL_PANIC {
            on_panic(NativePanic {
                code: ctx.trap,
                trace,
                message,
                pads: inner,
            });
        }
    }
}

/// Before native code calls the body at `pc` on `word`: whether to make
/// the call (`0`: a `drop` that ran already, as the VM's `CALL` skips it).
/// A claim before the commit is logged, so a bail gives it back.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_claim(ctx: *mut NativeCtx, pc: u64, word: u64) -> u64 {
    let c = unsafe { &mut *ctx };
    if c.machine.is_null() {
        return 1;
    }
    // SAFETY: `machine` is the live machine `host` was made for.
    let got = unsafe { (c.host.claim)(c.machine, pc as u32, word) };
    u64::from(got != 0)
}

/// `Panic` of the string `msg`: before a commit, bail (the VM's run panics);
/// after, a native panic with that message ([`TRAP_EXPLICIT`]).
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `msg` is a heap word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_panic(ctx: *mut NativeCtx, msg: u64) -> u64 {
    let c = unsafe { &mut *ctx };
    if let Some(crate::memory::Object::String(gc)) = object(msg) {
        c.trap = TRAP_EXPLICIT;
        // SAFETY: `call` set `message` for the call.
        unsafe { *c.message = gc.as_ref().data.as_str().to_string() };
    }
    unsafe { bail(ctx) }
}

/// `float %`.
#[unsafe(no_mangle)]
pub extern "C" fn coil_native_fmod(a: f64, b: f64) -> f64 {
    a % b
}

/// `float **`.
#[unsafe(no_mangle)]
pub extern "C" fn coil_native_powf(a: f64, b: f64) -> f64 {
    a.powf(b)
}

/// `int **` (`common::int_arith::pow`): the power goes to `out`; returns
/// `0`, or the trap code of its failure.
///
/// # Safety
///
/// `out` is a writable slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_ipow(a: i64, b: i64, out: *mut i64) -> i64 {
    match common::int_arith::pow(a, b) {
        Ok(v) => {
            // SAFETY: native code passes a slot of its frame.
            unsafe { *out = v };
            0
        }
        Err(common::int_arith::IntTrap::NegativeExponent) => TRAP_NEGATIVE_EXPONENT as i64,
        Err(_) => TRAP_OVERFLOW as i64,
    }
}

/// `VScan` / `VLoop` ops native code ran ([`coil_native_vector`]), for
/// `COIL_NATIVE_STATS`.
pub static VECTOR_OPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Bytecode runs native code started so far (a body, a host stub, a
/// generator, a `drop`): what a native run still needs the VM for.
pub static VM_ENTRIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Process-wide counts of what a native run still does on the VM, for the
/// native inventory (`COIL_NATIVE_INVENTORY`, `scripts/native_inventory.py`):
/// calls that bailed to a VM re-run, calls the VM ran because their body
/// has no native code, and host calls that ran in a VM stub.
pub static BAILS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// See [`BAILS`].
pub static VM_RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Bails before a commit by trap code ([`TRAP_HELPER`] `0`: an operation
/// native code does not model; [`TRAP_STACK`]: the stack budget), for the
/// inventory.
pub static BAIL_TRAPS: [std::sync::atomic::AtomicU64; 12] = [const { std::sync::atomic::AtomicU64::new(0) }; 12];
/// See [`BAILS`].
pub static HOST_VIA_VM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How a host call from native code ran, for the inventory.
#[derive(Clone, Copy)]
pub(crate) enum HostPath {
    /// On the runtime ([`crate::runtime::invoke_ordinary`]).
    Runtime,
    /// On the machine in Rust (task ops, regions): `NativeHost::invoke`.
    Machine,
    /// On a one-op VM stub ([`HOST_VIA_VM`]).
    Stub,
}

/// Host calls native code made, by host id and [`HostPath`].
static HOST_CALLS: [[std::sync::atomic::AtomicU64; 3]; 256] = [const { [const { std::sync::atomic::AtomicU64::new(0) }; 3] }; 256];

/// FFI ops native code ran on the machine (`FfiLoad`, `DeclareFFI`, a call).
static FFI_CALLS: [std::sync::atomic::AtomicU64; 3] = [const { std::sync::atomic::AtomicU64::new(0) }; 3];

/// Count a host call `id` that ran on `path`.
pub(crate) fn note_host_call(id: usize, path: HostPath) {
    if let Some(n) = HOST_CALLS.get(id) {
        n[path as usize].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The [`VM_ENTRIES`], [`BAILS`], [`VM_RUNS`] and [`HOST_VIA_VM`] counts,
/// then the non-zero [`BAIL_TRAPS`] as `bail_trap_<code>` and the
/// [`HOST_CALLS`] as `host_runtime:<name>` / `host_machine:<name>` /
/// `host_stub:<name>`.
pub fn vm_use_counts() -> Vec<(String, u64)> {
    use std::sync::atomic::Ordering::Relaxed;
    let mut out = vec![
        ("vm_entries".to_string(), VM_ENTRIES.load(Relaxed)),
        ("bails".to_string(), BAILS.load(Relaxed)),
        ("vm_runs".to_string(), VM_RUNS.load(Relaxed)),
        ("host_via_vm".to_string(), HOST_VIA_VM.load(Relaxed)),
    ];
    for (code, n) in BAIL_TRAPS.iter().enumerate() {
        let n = n.load(Relaxed);
        if n != 0 {
            out.push((format!("bail_trap_{code}"), n));
        }
    }
    for (op, n) in ["ffi_load", "ffi_declare", "ffi_call"].into_iter().zip(FFI_CALLS.iter().map(|n| n.load(Relaxed))) {
        if n != 0 {
            out.push((format!("host_runtime:{op}"), n));
        }
    }
    let hosts = direct_host_natives();
    for (id, paths) in HOST_CALLS.iter().enumerate() {
        let name = hosts.get(id).map_or_else(|| id.to_string(), |h| h.name.clone());
        for (kind, n) in ["runtime", "machine", "stub"].into_iter().zip(paths.iter().map(|n| n.load(Relaxed))) {
            if n != 0 {
                out.push((format!("host_{kind}:{name}"), n));
            }
        }
    }
    out
}

/// A `ByteScan` or `VecLoop` native code cannot reduce to lane code, run
/// by the runtime's lane kernels ([`crate::vm::lanes_on_frame`]). `kind` bit 0: a
/// loop (else a scan); bit 1: the loop writes the heap (it commits the
/// call first, so a trap after it panics here). `body` holds `nbody`
/// words `opcode | operand << 32`, `slots` the frame's `nslots` words.
/// Returns a scan's stop; a trap panics with the VM's message.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `body` and `slots` point at
/// `nbody` and `nslots` words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_vector(ctx: *mut NativeCtx, kind: u64, flags: u64, body: *const u64, nbody: u64, slots: *mut u64, nslots: u64, from: u64, bound: u64) -> u64 {
    let c = unsafe { &mut *ctx };
    let mut words = [common::Byte::default(); 128];
    let Some(code) = words.get_mut(..nbody as usize) else {
        return unsafe { bail(ctx) };
    };
    if c.heap.is_null() {
        return unsafe { bail(ctx) };
    }
    for (i, w) in code.iter_mut().enumerate() {
        // SAFETY: `body` holds `nbody` words.
        let x = unsafe { *body.add(i) };
        *w = common::Byte::new(common::Instruction::from(x as u8)).with_operand_u32((x >> 32) as u32);
    }
    // SAFETY: `slots` holds `nslots` words.
    let frame = unsafe { std::slice::from_raw_parts_mut(slots, nslots as usize) };
    // SAFETY: `call` set `heap` to the machine's live heap.
    let heap = unsafe { &mut *c.heap };
    match crate::vm::lanes_on_frame(heap, kind & 1 == 0, flags as u8, code, frame, from as i64, bound as i64) {
        Ok(p) => {
            VECTOR_OPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            p as u64
        }
        Err(msg) => {
            let c = unsafe { &mut *ctx };
            c.trap = TRAP_MESSAGE;
            // SAFETY: `call` set `message` for the call.
            unsafe { *c.message = msg.to_string() };
            unsafe { bail(ctx) }
        }
    }
}

/// Every runtime helper native code calls, by symbol name: the JIT binds
/// them, an AOT object imports them (the runtime library exports them
/// under these names).
pub fn helper_symbols() -> &'static [(&'static str, *const u8)] {
    struct Table([(&'static str, *const u8); HELPERS]);
    // SAFETY: function addresses, read-only.
    unsafe impl Sync for Table {}
    static TABLE: Table = Table([
        ("coil_native_fmod", coil_native_fmod as *const u8),
        ("coil_native_powf", coil_native_powf as *const u8),
        ("coil_native_ipow", coil_native_ipow as *const u8),
        ("coil_native_index", coil_native_index as *const u8),
        ("coil_native_len", coil_native_len as *const u8),
        ("coil_native_field", coil_native_field as *const u8),
        ("coil_native_store_index", coil_native_store_index as *const u8),
        ("coil_native_set_field", coil_native_set_field as *const u8),
        ("coil_native_view", coil_native_view as *const u8),
        ("coil_native_cow", coil_native_cow as *const u8),
        ("coil_native_cow_field", coil_native_cow_field as *const u8),
        ("coil_native_alloc", coil_native_alloc as *const u8),
        ("coil_native_array_push", coil_native_array_push as *const u8),
        ("coil_native_free_early", coil_native_free_early as *const u8),
        ("coil_native_format", coil_native_format as *const u8),
        ("coil_native_stringify", coil_native_stringify as *const u8),
        ("coil_native_eq", coil_native_eq as *const u8),
        ("coil_native_string", coil_native_string as *const u8),
        ("coil_native_enum_tag", coil_native_enum_tag as *const u8),
        ("coil_native_enum_payload", coil_native_enum_payload as *const u8),
        ("coil_native_unwind", coil_native_unwind as *const u8),
        ("coil_native_print", coil_native_print as *const u8),
        ("coil_native_host", coil_native_host as *const u8),
        ("coil_native_call_vm", coil_native_call_vm as *const u8),
        ("coil_native_call_value", coil_native_call_value as *const u8),
        ("coil_native_resume", coil_native_resume as *const u8),
        ("coil_native_coro_done", coil_native_coro_done as *const u8),
        ("coil_native_yield", coil_native_yield as *const u8),
        ("coil_native_ffi", coil_native_ffi as *const u8),
        ("coil_native_defer", coil_native_defer as *const u8),
        ("coil_native_claim", coil_native_claim as *const u8),
        ("coil_native_collect", coil_native_collect as *const u8),
        ("coil_native_panic", coil_native_panic as *const u8),
        ("coil_native_vector", coil_native_vector as *const u8),
    ]);
    &TABLE.0
}

const HELPERS: usize = 34;
