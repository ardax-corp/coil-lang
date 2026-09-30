//! Bytecode interpreter: dispatch loop, automatic GC, and FFI.

use std::{
    ffi::c_void,
    fmt::Write as FmtWrite,
    io::{self, Write as IoWrite},
    path::PathBuf,
    sync::Arc,
};

#[cfg(any(test, feature = "vm_profile"))]
use std::sync::atomic::{AtomicU64, Ordering};

use common::{
    byte_to_position, likely, promise, set_field_slot_index, unlikely, unpack_init_typed,
    ArchivedByte as Byte, ArchivedInstruction as Instruction, ArrayVec, Byte as RawByte,
    ProgramDebug, Value,
};

use crate::{
    AddrHashBuilder, CStructLayout, CoroState, EnumPayload, Frame, GcData, Heap, Member, ObjArray,
    ObjBoxed, ObjCoroutine, ObjEnum, ObjFn, ObjInstance, ObjPolyFn, ObjString, ObjTuple, Object,
    RefCoroutine, Stack,
};
#[cfg(any(test, feature = "debugger"))]
use crate::{DebugController, StopReason};
use common::ValueTag;

// Thread-local dispatch counter (tests / `vm_profile` only).
#[cfg(any(test, feature = "vm_profile"))]
thread_local! {
    static VM_DISPATCH_COUNT: AtomicU64 = const { AtomicU64::new(0) };
}

/// Reset the VM dispatch counter.
#[cfg(any(test, feature = "vm_profile"))]
pub fn reset_dispatch_count() {
    VM_DISPATCH_COUNT.with(|c| c.store(0, Ordering::Relaxed));
}

/// Read the VM dispatch counter.
#[cfg(any(test, feature = "vm_profile"))]
#[must_use]
pub fn dispatch_count() -> u64 {
    VM_DISPATCH_COUNT.with(|c| c.load(Ordering::Relaxed))
}

#[cfg(not(any(test, feature = "vm_profile")))]
#[must_use]
pub fn dispatch_count() -> u64 {
    0
}

#[cfg(not(any(test, feature = "vm_profile")))]
pub fn reset_dispatch_count() {}

// Per-PC dispatch histogram (tests / `vm_profile` only). Off until
// [`begin_pc_profile`] so ordinary test runs do not pay for it.
#[cfg(any(test, feature = "vm_profile"))]
thread_local! {
    static VM_PC_PROFILE: std::cell::RefCell<Option<Vec<u64>>> =
        const { std::cell::RefCell::new(None) };
}

/// Start counting dispatches per PC on this thread.
#[cfg(any(test, feature = "vm_profile"))]
pub fn begin_pc_profile() {
    VM_PC_PROFILE.with(|p| *p.borrow_mut() = Some(Vec::new()));
}

/// Stop profiling and return dispatch counts indexed by PC.
#[cfg(any(test, feature = "vm_profile"))]
pub fn take_pc_profile() -> Vec<u64> {
    VM_PC_PROFILE.with(|p| p.borrow_mut().take().unwrap_or_default())
}

// Frame-relative cursor (`stack.tell() - sp`) observed before each dispatch,
// paired with the PC. Feeds the differential test for the static cursor model
// in `compiler::il::tell`, which cannot be trusted from code reading alone.
#[cfg(any(test, feature = "vm_profile"))]
thread_local! {
    static VM_CURSOR_TRACE: std::cell::RefCell<Vec<(u32, u32)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Cap the trace so a long-running program cannot exhaust memory; a prefix is
/// still a valid check.
#[cfg(any(test, feature = "vm_profile"))]
const CURSOR_TRACE_CAP: usize = 400_000;

#[cfg(any(test, feature = "vm_profile"))]
pub fn reset_cursor_trace() {
    VM_CURSOR_TRACE.with(|t| t.borrow_mut().clear());
}

/// `(pc, frame_relative_cursor)` in dispatch order.
#[cfg(any(test, feature = "vm_profile"))]
#[must_use]
pub fn cursor_trace() -> Vec<(u32, u32)> {
    VM_CURSOR_TRACE.with(|t| t.borrow().clone())
}

#[cfg(not(any(test, feature = "vm_profile")))]
pub fn reset_cursor_trace() {}

#[cfg(not(any(test, feature = "vm_profile")))]
#[must_use]
pub fn cursor_trace() -> Vec<(u32, u32)> {
    Vec::new()
}

// Allocation / GC counters (`vm_profile` + tests). Useful for binary_trees-style
// heap traffic without needing external alloc tracers.
#[cfg(any(test, feature = "vm_profile"))]
thread_local! {
    static VM_ALLOC_COUNT: AtomicU64 = const { AtomicU64::new(0) };
    static VM_GC_COUNT: AtomicU64 = const { AtomicU64::new(0) };
    static VM_MAKE_FAST_COUNT: AtomicU64 = const { AtomicU64::new(0) };
    static VM_INTERN_STR_COUNT: AtomicU64 = const { AtomicU64::new(0) };
}

/// Record one managed heap object allocation.
#[cfg(any(test, feature = "vm_profile"))]
#[inline]
pub(crate) fn note_heap_alloc() {
    VM_ALLOC_COUNT.with(|c| {
        c.fetch_add(1, Ordering::Relaxed);
    });
}

#[cfg(not(any(test, feature = "vm_profile")))]
#[inline]
pub(crate) fn note_heap_alloc() {}

/// Reset allocation / GC / Make* fast-path counters.
#[cfg(any(test, feature = "vm_profile"))]
pub fn reset_alloc_profile() {
    VM_ALLOC_COUNT.with(|c| c.store(0, Ordering::Relaxed));
    VM_GC_COUNT.with(|c| c.store(0, Ordering::Relaxed));
    VM_MAKE_FAST_COUNT.with(|c| c.store(0, Ordering::Relaxed));
    VM_INTERN_STR_COUNT.with(|c| c.store(0, Ordering::Relaxed));
}

#[cfg(not(any(test, feature = "vm_profile")))]
pub fn reset_alloc_profile() {}

/// Number of managed objects allocated since the last reset.
#[cfg(any(test, feature = "vm_profile"))]
#[must_use]
pub fn alloc_count() -> u64 {
    VM_ALLOC_COUNT.with(|c| c.load(Ordering::Relaxed))
}

#[cfg(not(any(test, feature = "vm_profile")))]
#[must_use]
pub fn alloc_count() -> u64 {
    0
}

/// Number of mark-and-sweep collections since the last reset.
#[cfg(any(test, feature = "vm_profile"))]
#[must_use]
pub fn gc_count() -> u64 {
    VM_GC_COUNT.with(|c| c.load(Ordering::Relaxed))
}

#[cfg(not(any(test, feature = "vm_profile")))]
#[must_use]
pub fn gc_count() -> u64 {
    0
}

/// Number of MakeTuple / MakeArray / MakeEnum fixed-arity fast paths taken.
#[cfg(any(test, feature = "vm_profile"))]
#[must_use]
pub fn make_fast_count() -> u64 {
    VM_MAKE_FAST_COUNT.with(|c| c.load(Ordering::Relaxed))
}

#[cfg(not(any(test, feature = "vm_profile")))]
#[must_use]
pub fn make_fast_count() -> u64 {
    0
}

#[cfg(any(test, feature = "vm_profile"))]
#[inline]
fn note_make_fast() {
    VM_MAKE_FAST_COUNT.with(|c| {
        c.fetch_add(1, Ordering::Relaxed);
    });
}

#[cfg(not(any(test, feature = "vm_profile")))]
#[inline]
fn note_make_fast() {}

/// Record one `Heap::intern_str` (hash + intern table probe).
#[cfg(any(test, feature = "vm_profile"))]
#[inline]
pub(crate) fn note_intern_str() {
    VM_INTERN_STR_COUNT.with(|c| {
        c.fetch_add(1, Ordering::Relaxed);
    });
}

#[cfg(not(any(test, feature = "vm_profile")))]
#[inline]
pub(crate) fn note_intern_str() {}

/// Number of `intern_str` calls since the last reset.
#[cfg(any(test, feature = "vm_profile"))]
#[must_use]
pub fn intern_str_count() -> u64 {
    VM_INTERN_STR_COUNT.with(|c| c.load(Ordering::Relaxed))
}

#[cfg(not(any(test, feature = "vm_profile")))]
#[must_use]
pub fn intern_str_count() -> u64 {
    0
}

macro_rules! binary {
    ($stack: expr, $op:tt, $from: ident, $to: ident) => {
        {
            let sp = $stack.tell();
            promise!(sp >= 2);
            let rhs_idx = sp - 1;
            let lhs_idx = sp - 2;
            let rhs = $stack[rhs_idx].$from();
            let lhs = $stack[lhs_idx].$from();
            $stack[lhs_idx].replace((lhs $op rhs).$to());
            $stack.seek(lhs_idx + 1);
        }
    };
    ($stack: expr, $op:tt, $from: ident) => {
        {
            let sp = $stack.tell();
            promise!(sp >= 2);
            let rhs_idx = sp - 1;
            let lhs_idx = sp - 2;
            let rhs = $stack[rhs_idx].$from();
            let lhs = $stack[lhs_idx].$from();
            $stack[lhs_idx].replace((lhs $op rhs) as _);
            $stack.seek(lhs_idx + 1);
        }
    };
}

macro_rules! unary {
    ($stack: expr, $op: tt, $from: ident, $to: ident) => {
        {
            let sp = $stack.tell();
            promise!(sp >= 1);
            let idx = sp - 1;
            let rhs = $stack[idx].$from();
            $stack[idx].replace(($op rhs).$to());
        }
    };
    ($stack: expr, $op: tt, $from: ident) => {
        {
            let sp = $stack.tell();
            promise!(sp >= 1);
            let idx = sp - 1;
            let rhs = $stack[idx].$from();
            $stack[idx].replace(($op rhs) as _);
        }
    };
}

/// Previously prefetched `code[ip]` (`prefetcht1` / `prfm`) after a
/// `ip >= len` check. On the flagship loops the next word is already in L1,
/// and the check plus the prefetch retired on every dispatch. Call sites stay
/// so a later measurement can turn it back on in one place.
#[inline(always)]
fn prefetch_code(_code: &[Byte], _ip: usize) {}

thread_local! {
    /// Active `execute` invocations on this thread. More than one means a
    /// host frame (native re-entry via `call_function`, a finalizer) sits
    /// between VM frames and may hold raw heap handles, so evacuation must
    /// not move. Kept out of `Machine` so its hot-loop field layout is
    /// unchanged.
    static IN_EXECUTE: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// Per static slot word kind of the program installed on this thread
    /// (archive minor 29). Only GC root walks read it; kept out of `Machine`
    /// / `Heap` so their hot-loop layout is unchanged.
    static STATIC_KINDS: std::cell::RefCell<Arc<Vec<u8>>> = std::cell::RefCell::new(Arc::default());
    /// Evacuation pacing for this thread's VM: `(cycles since the last
    /// attempt, collections to wait)`. Also outside `Machine` / `Heap`.
    static COMPACT_PACING: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, COMPACT_WINDOW)) };
    /// Off with `COIL_GC_COMPACT=0` (debugging a suspected move bug).
    static COMPACT_ENABLED: bool = std::env::var_os("COIL_GC_COMPACT").is_none_or(|v| v != "0");
}

/// Collections between evacuation attempts outside stress mode, and the
/// ceiling unproductive attempts back off to.
const COMPACT_WINDOW: u32 = 8;
const COMPACT_WINDOW_MAX: u32 = 256;

#[inline(always)]
fn set_jump_target(ip: &mut usize, target: usize, code: &[Byte]) {
    // Lowering may target `code.len()` as “fall out of the loop” (next `while`
    // check exits without fetching).
    promise!(target <= code.len());
    *ip = target;
    prefetch_code(code, target);
}

#[inline(always)]
fn note_dispatch_at(ip: usize, stack: &Stack<Value>, sp: usize) {
    #[cfg(any(test, feature = "vm_profile"))]
    {
        VM_DISPATCH_COUNT.with(|c| c.fetch_add(1, Ordering::Relaxed));
        VM_PC_PROFILE.with(|p| {
            if let Some(counts) = p.borrow_mut().as_mut() {
                if counts.len() <= ip {
                    counts.resize(ip + 1, 0);
                }
                counts[ip] += 1;
            }
        });
        VM_CURSOR_TRACE.with(|t| {
            let mut t = t.borrow_mut();
            if t.len() < CURSOR_TRACE_CAP {
                t.push((ip as u32, (stack.tell().saturating_sub(sp)) as u32));
            }
        });
    }
    #[cfg(not(any(test, feature = "vm_profile")))]
    {
        let _ = (ip, stack, sp);
    }
}

#[path = "dispatch.rs"]
mod dispatch;

// type External = fn(&[Value]) -> Value;

type OutputSink = Box<dyn IoWrite + Send>;

/// Saved resumer context while a coroutine runs on the shared stack.
#[derive(Clone, Copy)]
struct ResumeCtx {
    coro: RefCoroutine,
    base_sp: usize,
    frame_depth: usize,
}

/// Deferred `FfiInvoke` so libffi (and callbacks) run outside `execute`'s borrow.
struct PendingFfiInvoke {
    lib_addr: u64,
    function_id: usize,
    args: Vec<Value>,
    /// Per-arg FFI type tags for variadic calls (`None` when fixed-arity).
    arg_types: Option<Vec<crate::memory::FfiType>>,
    resume_ip: usize,
    resume_sp: usize,
}

/// Parked HostInvoke waiting on IO readiness (`await_readable` / `await_writable`).
struct PendingIoWait {
    request: crate::io::IoParkRequest,
    resume_ip: usize,
    resume_sp: usize,
    /// Layout from the parked `HostInvoke` operand. Resume must pack with this
    /// so `Result<(), IoError>` match sees OptionNiche `Ok` = `0`, not a box.
    layout: crate::host_enum::HostEnumLayout,
}

/// One frame's pinned arrays, keyed by local slot (`ArrayPin` operand or
/// `DenseIndex` / `DenseStoreIndex` array register).
///
/// Allocated lazily on first pin. Lookup is a vec index, not a hash.
struct FramePins {
    /// `frames.len()` at first pin (TailCall keeps the same depth).
    depth: usize,
    by_slot: Vec<Option<Object>>,
}

#[inline]
fn pinned_object_in(frame_pins: &[FramePins], frames_len: usize, slot: u32) -> Option<Object> {
    let pins = frame_pins.last()?;
    if pins.depth != frames_len {
        return None;
    }
    pins.by_slot.get(slot as usize).copied().flatten()
}

#[inline]
fn pinned_object_matching_in(
    frame_pins: &[FramePins],
    frames_len: usize,
    slot: u32,
    addr: u64,
) -> Option<Object> {
    let obj = pinned_object_in(frame_pins, frames_len, slot)?;
    if obj.addr() == addr {
        Some(obj)
    } else {
        None
    }
}

#[inline]
fn pin_current_array_in(
    frame_pins: &mut Vec<FramePins>,
    frames_len: usize,
    slot: u32,
    obj: Object,
) {
    let idx = slot as usize;
    if let Some(pins) = frame_pins.last_mut()
        && pins.depth == frames_len {
            if pins.by_slot.len() <= idx {
                pins.by_slot.resize(idx + 1, None);
            }
            pins.by_slot[idx] = Some(obj);
            return;
        }
    let mut by_slot = vec![None; idx + 1];
    by_slot[idx] = Some(obj);
    frame_pins.push(FramePins {
        depth: frames_len,
        by_slot,
    });
}

/// Reuse a cached `Object` while the array address is unchanged (COI-372).
#[inline(always)]
fn resolve_dense_index_object_in(
    heap: &Heap,
    frames_len: usize,
    frame_pins: &mut Vec<FramePins>,
    dense_obj_addr: &mut u64,
    dense_obj: &mut Option<Object>,
    slot: u32,
    addr: u64,
) -> Option<Object> {
    if likely(addr != 0 && addr == *dense_obj_addr) {
        return *dense_obj;
    }
    if let Some(obj) = pinned_object_matching_in(frame_pins, frames_len, slot, addr) {
        *dense_obj_addr = addr;
        *dense_obj = Some(obj);
        return Some(obj);
    }
    let obj = heap.find_object_by_addr(addr)?;
    if matches!(obj, Object::Array(_) | Object::Tuple(_)) {
        pin_current_array_in(frame_pins, frames_len, slot, obj);
        *dense_obj_addr = addr;
        *dense_obj = Some(obj);
    }
    Some(obj)
}

/// Panic when a frame needs more operand stack than [`crate::MAX_OPERAND_STACK_SLOTS`].
const STACK_OVERFLOW: &str = "stack overflow: recursion exceeds the VM's operand stack limit";

/// Panic when [`Machine::set_step_budget`]'s budget runs out.
pub(crate) const STEP_BUDGET_EXHAUSTED: &str =
    "step budget exhausted: execution exceeded its back-edge / call limit";

/// Call depths [`Machine::rearm_call_window`] reserves ahead of the current one.
const CALL_WINDOW: usize = 32;

pub struct Machine<const S: usize> {
    heap: crate::memory::HeapSlot,
    stack: Stack<Value>,
    frames: ArrayVec<Frame, S>,
    /// Pin tables for frames that ran `ArrayPin` or a dense heap index.
    frame_pins: Vec<FramePins>,
    /// Last dense-index array identity (predicted hit in counted loops).
    dense_obj_addr: u64,
    dense_obj: Option<Object>,
    output: Option<OutputSink>,
    natives: crate::ffi::Natives,
    libraries: std::collections::HashMap<String, std::sync::Arc<crate::ffi::Library>>,
    userland_libraries: std::collections::HashMap<u64, Object, AddrHashBuilder>,
    resume_stack: Vec<ResumeCtx>,
    /// Directory of the entry script (for relative `dload` paths).
    base_dir: Option<PathBuf>,
    /// Extra search paths from `coil.toml` `[ffi]`.
    ffi_search_paths: Vec<PathBuf>,
    /// Fail-closed `dload` integrity (lock hash or trusted).
    dload_gate: crate::ffi::DloadGate,
    /// Registered C struct layouts for pass-by-value FFI.
    /// Shared with spawned jobs (a refcount per spawn, not a copy).
    struct_layouts: Arc<Vec<CStructLayout>>,
    /// Keeps libffi callback trampolines alive (ties lifetime to VM run).
    ffi_closures: Vec<crate::ffi::OwnedClosure>,
    /// Bytecode/constants for nested `call_function` / callbacks.
    /// `Arc` so reactor workers share the [`crate::thread::ThreadProgram`] image.
    program_code: Arc<Vec<RawByte>>,
    program_constants: Arc<Vec<u64>>,
    program_strings: Arc<Vec<String>>,
    /// Interned handle per `program_strings` index. Not a GC root: sweep
    /// zeros the table so unmarked literals can die; STRING still stacks
    /// the handle before `maybe_gc`.
    program_string_cache: Vec<Value>,
    /// When > 0, `RETURN` captures into `nested_return` instead of unwinding to caller.
    nested_depth: u32,
    /// Set when a `RETURN` must update pins, a nested host call, or a
    /// coroutine. Fib, tak, and binary trees leave it clear, so those
    /// returns skip the three empty checks.
    return_bookkeeping: bool,
    /// Stack of frame-stack lengths at each active [`call_function`] entry.
    /// Only a `RETURN` that pops back to `last()` should capture `nested_return`
    /// (inner `CALL`s must still unwind normally). A stack (not a scalar) is
    /// required so nested `call_function` reentrancy (FFI callbacks) does not
    /// overwrite the outer depth.
    nested_frame_depths: Vec<usize>,
    nested_return: Option<Value>,
    /// Set when `execute` pauses before a native FFI call that may reenter the VM.
    pending_ffi: Option<PendingFfiInvoke>,
    /// Set when `await_*` parks until fd readiness (CPU help-steals meanwhile).
    pending_io: Option<PendingIoWait>,
    /// Set when a language-level `panic` aborts the VM.
    panicked: bool,
    /// Global static slots (`LoadStatic` / `StoreStatic`).
    statics: Vec<Value>,
    /// Debug line table (parallel to archived bytecode indices).
    program_debug: ProgramDebug,
    /// Cached `(file_index, line)` per PC for debug stepping (built from `program_debug`).
    pc_lines: Vec<Option<(u32, u32)>>,
    #[cfg(any(test, feature = "debugger"))]
    /// Optional debug controller; when set, `execute` may pause at stops.
    debug: Option<Box<DebugController>>,
    #[cfg(any(test, feature = "debugger"))]
    /// Set when `execute` pauses for the debugger (alongside `pending_ffi`).
    pending_debug_stop: Option<StopReason>,
    #[cfg(any(test, feature = "coverage"))]
    /// Hit count per PC while coverage is on (see [`Self::begin_coverage`]).
    coverage: Option<Vec<u32>>,
    /// Shared program image for OS thread workers (`spawn`).
    thread_program: Option<std::sync::Arc<crate::thread::ThreadProgram>>,
    /// Optional shared stdout capture for worker threads.
    shared_print: Option<std::sync::Arc<std::sync::Mutex<Vec<u8>>>>,
    /// Undetached spawns owned by this VM (joined at end of `run_with_pool`).
    live_threads: crate::thread::LiveThreadRegistry,
    /// Shared concurrent OS-worker budget for this root VM (and its workers).
    worker_cap: std::sync::Arc<crate::thread::WorkerCap>,
    /// Work-stealing pool sized by [`Self::worker_cap`].
    reactor: std::sync::Arc<crate::reactor::Reactor>,
    /// IO readiness reactor (sync adapters + async waiters).
    io_reactor: std::sync::Arc<crate::io_reactor::IoReactor>,
    /// S2b maps: live heap IL slots at alloc safepoints.
    stack_maps: Vec<common::FrameStackMap>,
    /// Complete frame maps (sorted by entry); see [`common::PreciseFrameMap`].
    precise_frames: Arc<Vec<common::PreciseFrameMap>>,
    /// PC past the op that entered the current GC safepoint, while one runs.
    /// `None` keeps the top frame on the conservative scan.
    gc_top_ip: Option<usize>,
    /// Bytecode PC of the current GC safepoint (alloc / `gc::collect`).
    gc_ip: usize,
    /// Compiler-only SIMD file (numeric bits only; never GC-traced).
    vregs: [[u64; common::simd::LANES]; common::simd::NREGS],
    /// `type_id` → drop method entry PC (empty = no user finalizers).
    finalizer_by_type: std::collections::HashMap<u32, u32, AddrHashBuilder>,
    /// Drop entry PCs (for explicit `obj.drop()` once-bit intercept).
    finalizer_pcs: std::collections::HashSet<u32, AddrHashBuilder>,
    /// True while a mark/finalize/sweep cycle is running.
    gc_in_progress: bool,
    /// Nested `gc_collect` during a finalizer; run another cycle after.
    gc_deferred: bool,
    /// Live C1/C2 steal epoch (root). Helpers bind via [`HeapSlot`].
    shared_epoch: Option<std::sync::Arc<crate::shared_heap::SharedHeapEpoch>>,
    /// Join result bits not yet stored on the operand stack (Layer A collect).
    steal_join_root: Value,
    /// [`common::FrameReserve`] of the code `execute` last ran, keyed by that
    /// code's address and length. Every frame open keeps `frame_reserve`
    /// words free.
    frame_reserve: usize,
    match_payload_bound: usize,
    frame_reserve_key: (usize, usize),
    /// Constant base cases of unary callees, for the same code as
    /// `frame_reserve_key`.
    unary_bases: dispatch::UnaryBaseTable,
    /// `CALL` may open a frame without a stack check while fewer frames than
    /// this are live (see [`Self::rearm_call_window`]).
    call_hot_depth: usize,
    /// Stack end the current window was reserved up to (`<= capacity`).
    call_window_end: usize,
    /// Step budget fuel: charged once per taken back-edge (jump target `<=`
    /// the jump's PC) and per call; zero means exhausted. `u64::MAX` when no
    /// budget is set (2^64 charges cannot happen), so the hot path is one
    /// decrement and one never-taken branch either way.
    fuel: u64,
    /// Set once the budget ran out (see [`Self::step_budget_exhausted`]).
    step_budget_hit: bool,
}

impl<const S: usize> Default for Machine<S> {
    fn default() -> Self {
        Self::with_operand_capacity(crate::DEFAULT_OPERAND_STACK_SLOTS)
    }
}

impl<const S: usize> Machine<S> {
    /// Build a VM with a program-specific operand-stack capacity.
    pub fn with_operand_capacity(operand_slots: usize) -> Self {
        let mut frames = ArrayVec::default();
        frames.consume();
        let worker_cap = crate::thread::WorkerCap::new();
        let reactor = crate::reactor::Reactor::new(0);
        let cap = operand_slots.clamp(1, crate::MAX_OPERAND_STACK_SLOTS);
        Self {
            frames,
            frame_pins: Vec::new(),
            dense_obj_addr: 0,
            dense_obj: None,
            heap: crate::memory::HeapSlot::default(),
            stack: Stack::with_capacity(cap),
            output: None,
            natives: crate::ffi::Natives::new(),
            libraries: std::collections::HashMap::new(),
            userland_libraries: std::collections::HashMap::default(),
            resume_stack: Vec::new(),
            base_dir: None,
            ffi_search_paths: Vec::new(),
            dload_gate: crate::ffi::DloadGate::deny_all(),
            struct_layouts: Arc::default(),
            ffi_closures: Vec::new(),
            program_code: Arc::new(Vec::new()),
            program_constants: Arc::new(Vec::new()),
            program_strings: Arc::new(Vec::new()),
            program_string_cache: Vec::new(),
            nested_depth: 0,
            return_bookkeeping: false,
            nested_frame_depths: Vec::new(),
            nested_return: None,
            pending_ffi: None,
            pending_io: None,
            panicked: false,
            statics: Vec::new(),
            program_debug: ProgramDebug::default(),
            pc_lines: Vec::new(),
            #[cfg(any(test, feature = "debugger"))]
            debug: None,
            #[cfg(any(test, feature = "debugger"))]
            pending_debug_stop: None,
            #[cfg(any(test, feature = "coverage"))]
            coverage: None,
            thread_program: None,
            shared_print: None,
            live_threads: crate::thread::new_live_thread_registry(),
            worker_cap,
            reactor,
            io_reactor: crate::io_reactor::IoReactor::new(),
            stack_maps: Vec::new(),
            precise_frames: Arc::default(),
            gc_top_ip: None,
            gc_ip: 0,
            vregs: [[0u64; common::simd::LANES]; common::simd::NREGS],
            finalizer_by_type: std::collections::HashMap::default(),
            finalizer_pcs: std::collections::HashSet::default(),
            gc_in_progress: false,
            gc_deferred: false,
            shared_epoch: None,
            steal_join_root: Value::default(),
            frame_reserve: 0,
            match_payload_bound: 0,
            frame_reserve_key: (0, 0),
            unary_bases: dispatch::UnaryBaseTable::default(),
            call_hot_depth: 0,
            call_window_end: 0,
            fuel: u64::MAX,
            step_budget_hit: false,
        }
    }

    /// Limit execution to `budget` steps, where a step is a taken loop
    /// back-edge (a jump whose target PC is at or before the jump) or a call
    /// (`CALL`, `TailCall`, `CallIndirect`, closure / polymorphic calls, and
    /// [`Self::call_function`] re-entry). Running out is a VM panic
    /// ([`Self::panicked`] and [`Self::step_budget_exhausted`] become true).
    /// `None` (the default) removes the limit. The budget spans every run on
    /// this machine until set again; spawned worker machines never inherit it.
    pub fn set_step_budget(&mut self, budget: Option<u64>) {
        // `budget` charges succeed; the next one reaches zero.
        self.fuel = budget.map_or(u64::MAX, |b| b.saturating_add(1));
        self.step_budget_hit = false;
    }

    /// True once the budget from [`Self::set_step_budget`] ran out.
    pub fn step_budget_exhausted(&self) -> bool {
        self.step_budget_hit
    }

    /// Fuel reached zero: panic with [`STEP_BUDGET_EXHAUSTED`] at `pc`.
    /// Returns `false` (stop), like [`Self::runtime_panic`]. Leaves one unit
    /// so any later charge (host re-entry after the panic) stops again.
    #[cold]
    #[inline(never)]
    fn step_budget_out(&mut self, pc: usize) -> bool {
        self.fuel = 1;
        if self.step_budget_hit {
            self.panicked = true;
            return false;
        }
        self.step_budget_hit = true;
        self.runtime_panic(STEP_BUDGET_EXHAUSTED, pc)
    }

    /// Panic reported by a dense streak: the streak stops with fuel at zero
    /// only when it ran out of step budget.
    #[cold]
    #[inline(never)]
    fn hot_streak_panic(&mut self, msg: &'static str, pc: usize) -> bool {
        if self.fuel == 0 {
            return self.step_budget_out(pc);
        }
        self.runtime_panic(msg, pc)
    }

    /// Current operand-stack capacity (slots).
    pub fn operand_stack_capacity(&self) -> usize {
        self.stack.capacity()
    }

    pub fn set_ffi_paths(&mut self, base_dir: Option<PathBuf>, search_paths: Vec<PathBuf>) {
        self.base_dir = base_dir;
        self.ffi_search_paths = search_paths;
    }

    /// Replace the `dload` gate (default is deny-all).
    pub fn set_dload_gate(&mut self, gate: crate::ffi::DloadGate) {
        self.dload_gate = gate;
    }

    pub fn dload_gate(&self) -> &crate::ffi::DloadGate {
        &self.dload_gate
    }

    /// Host/test stems with no lock hash. Does not restore a first-party exemption.
    pub fn set_dload_allowlist<I, St>(&mut self, extra_stems: I)
    where
        I: IntoIterator<Item = St>,
        St: AsRef<str>,
    {
        for stem in extra_stems {
            self.dload_gate.grant_stem(stem.as_ref());
        }
    }

    /// Mutable access for host/test grants after [`Self::set_dload_gate`].
    pub fn dload_gate_mut(&mut self) -> &mut crate::ffi::DloadGate {
        &mut self.dload_gate
    }

    pub fn set_program_debug(&mut self, debug: ProgramDebug) {
        self.program_debug = debug;
        self.rebuild_pc_line_cache();
    }

    /// Start counting executed instructions per PC (clears earlier counts).
    /// Dense streaks are off while counting, so every instruction is seen.
    #[cfg(any(test, feature = "coverage"))]
    pub fn begin_coverage(&mut self) {
        self.coverage = Some(vec![0; self.program_code.len()]);
    }

    #[cfg(any(test, feature = "coverage"))]
    #[cold]
    fn note_coverage(&mut self, ip: usize) {
        if let Some(counts) = self.coverage.as_mut() {
            if counts.len() <= ip {
                counts.resize(ip + 1, 0);
            }
            counts[ip] = counts[ip].saturating_add(1);
        }
    }

    /// Stop counting and return hit counts indexed by PC (`None` if off).
    #[cfg(any(test, feature = "coverage"))]
    pub fn take_coverage(&mut self) -> Option<Vec<u32>> {
        self.coverage.take()
    }

    /// Attach a debug controller (enables stop checks in `execute`).
    #[cfg(any(test, feature = "debugger"))]
    pub fn attach_debug(&mut self, controller: DebugController) {
        self.debug = Some(Box::new(controller));
        self.pending_debug_stop = None;
        if self.pc_lines.is_empty() {
            self.rebuild_pc_line_cache();
        }
    }

    /// Borrow the attached debug controller, if any.
    #[cfg(any(test, feature = "debugger"))]
    pub fn debug_controller_mut(&mut self) -> Option<&mut DebugController> {
        self.debug.as_deref_mut()
    }

    #[cfg(any(test, feature = "debugger"))]
    pub fn debug_controller(&self) -> Option<&DebugController> {
        self.debug.as_deref()
    }

    #[cfg(any(test, feature = "debugger"))]
    pub fn debug_is_attached(&self) -> bool {
        self.debug.is_some()
    }

    fn rebuild_pc_line_cache(&mut self) {
        use std::collections::HashMap;
        let mut texts: HashMap<u32, String, AddrHashBuilder> = HashMap::default();
        self.pc_lines.clear();
        self.pc_lines.reserve(self.program_debug.debug_locs.len());
        for loc in &self.program_debug.debug_locs {
            if !loc.is_known() {
                self.pc_lines.push(None);
                continue;
            }
            let text = texts.entry(loc.file).or_insert_with(|| {
                let path = self
                    .program_debug
                    .source_files
                    .get(loc.file as usize)
                    .map(|p| self.resolve_source_path(p))
                    .unwrap_or_default();
                std::fs::read_to_string(path).unwrap_or_default()
            });
            if text.is_empty() {
                self.pc_lines.push(None);
                continue;
            }
            let pos = byte_to_position(text, loc.start_byte as usize);
            self.pc_lines.push(Some((loc.file, pos.line)));
        }
    }

    /// Resolve PC → `(path, line, column)` when debug locs are known.
    pub fn resolve_pc_location(&self, pc: usize) -> Option<(String, u32, u32)> {
        let loc = self.program_debug.debug_locs.get(pc)?;
        if !loc.is_known() {
            return None;
        }
        let path = self.program_debug.source_files.get(loc.file as usize)?;
        let resolved = self.resolve_source_path(path);
        let text = std::fs::read_to_string(&resolved).ok()?;
        let pos = byte_to_position(&text, loc.start_byte as usize);
        Some((resolved.display().to_string(), pos.line, pos.column))
    }

    pub fn debug_ip(&self) -> usize {
        if self.frames.is_empty() {
            return 0;
        }
        self.frames.get().tell()
    }

    pub fn debug_frame_depth(&self) -> usize {
        self.frames.len()
    }

    pub fn debug_frame_sp(&self, frame_idx: usize) -> Option<usize> {
        if frame_idx >= self.frames.len() {
            return None;
        }
        Some(self.frames[frame_idx].get())
    }

    pub fn debug_frame_ip(&self, frame_idx: usize) -> Option<usize> {
        if frame_idx >= self.frames.len() {
            return None;
        }
        Some(self.frames[frame_idx].tell())
    }

    /// Read local/operand slot `slot` relative to frame base (`frame.sp + slot`).
    pub fn debug_slot(&self, frame_idx: usize, slot: usize) -> Option<Value> {
        let base = self.debug_frame_sp(frame_idx)?;
        let idx = base + slot;
        let cap = self.stack.capacity();
        if idx >= self.stack.tell() && idx >= cap {
            return None;
        }
        // Allow reading within the stack buffer even past cursor for allocated locals.
        if idx >= cap {
            return None;
        }
        Some(self.stack[idx])
    }

    /// The heap object at `v` (a bare or `| 1`-tagged address), if any.
    #[cfg(any(test, feature = "debugger"))]
    pub fn debug_object(&self, v: Value) -> Option<crate::debug::DebugObject> {
        use crate::debug::DebugObject;
        use crate::memory::Object;
        let addr = v.raw() as u64 & !1;
        if addr == 0 {
            return None;
        }
        Some(match Self::find_object_by_addr(&self.heap, addr)? {
            Object::String(gc) => DebugObject::Str(gc.as_ref().data.clone()),
            Object::Instance(gc) => {
                let inst = gc.as_ref();
                match inst.slots() {
                    Some(fields) => DebugObject::Instance {
                        type_id: inst.type_id,
                        fields: fields.to_vec(),
                    },
                    None => DebugObject::Other("dict"),
                }
            }
            Object::Array(gc) => DebugObject::Array(gc.as_ref().elements().clone()),
            Object::Tuple(gc) => DebugObject::Tuple(gc.as_ref().elements().to_vec()),
            Object::Enum(gc) => {
                let e = gc.as_ref();
                DebugObject::Enum {
                    tag: e.tag,
                    payload: e.payload.to_vec(),
                }
            }
            Object::Boxed(gc) => match &gc.as_ref().payload {
                crate::memory::Member::Value(inner) => DebugObject::Boxed(*inner),
                _ => DebugObject::Other("boxed"),
            },
            Object::Coroutine(_) => DebugObject::Other("coroutine"),
            Object::Fn(_) | Object::PolyFn(_) => DebugObject::Other("fn"),
            _ => DebugObject::Other("object"),
        })
    }

    pub fn debug_format_value(&self, v: Value) -> String {
        Self::stringify_value(&self.heap, v)
    }

    pub fn program_debug(&self) -> &ProgramDebug {
        &self.program_debug
    }

    /// Cached `(file_index, line)` for a PC, if known.
    pub fn debug_pc_line(&self, pc: usize) -> Option<(u32, u32)> {
        self.pc_lines.get(pc).copied().flatten()
    }

    /// Reset execution state for a fresh `run` (keeps natives / debug / program_debug).
    #[cfg(any(test, feature = "debugger"))]
    pub fn debug_reset(&mut self) {
        self.stack = Stack::with_capacity(self.stack.capacity());
        self.frames = ArrayVec::default();
        self.frames.consume();
        self.frame_pins.clear();
        self.clear_dense_obj_cache();
        self.panicked = false;
        self.pending_ffi = None;
        self.pending_io = None;
        self.pending_debug_stop = None;
        self.nested_depth = 0;
        self.nested_frame_depths.clear();
        self.nested_return = None;
        self.resume_stack.clear();
        self.return_bookkeeping = !self.frame_pins.is_empty();
        self.statics.clear();
        if let Some(dbg) = self.debug.as_mut() {
            dbg.clear_step();
            dbg.clear_skip_bp();
        }
    }

    #[cfg(any(test, feature = "debugger"))]
    fn debug_check_stop_at(&mut self, ip: usize) -> Option<StopReason> {
        let depth = self.frames.len();
        let loc = self.pc_lines.get(ip).copied().flatten();
        self.debug.as_mut()?.check_stop(ip, depth, loc)
    }

    /// Run until the next debug stop, halt, or panic. Auto-resumes FFI pauses.
    #[cfg(any(test, feature = "debugger"))]
    pub fn debug_run_until(
        &mut self,
        code: &[Byte],
        constants: &[u64],
        strings: &[String],
        static_slots: u32,
        start_ip: usize,
    ) -> StopReason {
        if code.is_empty() {
            return StopReason::Halt;
        }
        if self.statics.len() != static_slots as usize {
            self.statics = vec![Value::default(); static_slots as usize];
        }
        if self.program_code.is_empty() {
            self.program_code = Arc::new(unsafe {
                std::slice::from_raw_parts(code.as_ptr().cast::<RawByte>(), code.len()).to_vec()
            });
            self.program_constants = Arc::new(constants.to_vec());
            self.install_program_strings(strings);
            self.sync_thread_program_from_current();
        }
        let mut ip = start_ip;
        loop {
            self.pending_debug_stop = None;
            let paused = self.run_execute(code, constants, ip);
            if let Some(pending) = self.pending_ffi.take() {
                let resume_ip = pending.resume_ip;
                self.finish_pending_ffi_invoke(pending);
                ip = resume_ip;
                continue;
            }
            if let Some(pending) = self.pending_io.take() {
                let resume_ip = pending.resume_ip;
                self.finish_pending_io_wait(pending);
                ip = resume_ip;
                continue;
            }
            if let Some(reason) = self.pending_debug_stop.take() {
                return reason;
            }
            if self.panicked {
                return StopReason::Panic;
            }
            if !paused {
                return StopReason::Halt;
            }
            return StopReason::Halt;
        }
    }

    /// Like [`debug_run_until`] for compiler-owned [`RawByte`] buffers.
    #[cfg(any(test, feature = "debugger"))]
    pub fn debug_run_until_raw(
        &mut self,
        code: &[RawByte],
        constants: &[u64],
        strings: &[String],
        static_slots: u32,
        start_ip: usize,
    ) -> StopReason {
        let code: &[Byte] = unsafe { std::slice::from_raw_parts(code.as_ptr().cast(), code.len()) };
        self.debug_run_until(code, constants, strings, static_slots, start_ip)
    }

    fn resolve_source_path(&self, path: &str) -> PathBuf {
        let p = PathBuf::from(path);
        if p.is_absolute() {
            return p;
        }
        if std::fs::metadata(&p).is_ok() {
            return p;
        }
        if let Some(base) = &self.base_dir {
            let root = base.parent().unwrap_or(base.as_path());
            let from_root = root.join(path);
            if std::fs::metadata(&from_root).is_ok() {
                return from_root;
            }
            let from_base = base.join(path);
            if std::fs::metadata(&from_base).is_ok() {
                return from_base;
            }
        }
        p
    }

    fn format_panic_location(&self, panic_insn_ip: usize) -> Option<String> {
        let loc = self.program_debug.debug_locs.get(panic_insn_ip)?;
        if !loc.is_known() {
            return None;
        }
        let path = self.program_debug.source_files.get(loc.file as usize)?;
        let read_path = self.resolve_source_path(path);
        let text = std::fs::read_to_string(&read_path).ok()?;
        let pos = byte_to_position(&text, loc.start_byte as usize);
        Some(format!("{}:{}:{}", path, pos.line, pos.column))
    }

    fn fn_symbol_at_ip(&self, ip: usize) -> Option<&str> {
        let syms = &self.program_debug.fn_symbols;
        if syms.is_empty() {
            return None;
        }
        let mut lo = 0usize;
        let mut hi = syms.len();
        let mut best = None;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if syms[mid].entry_pc as usize <= ip {
                best = Some(mid);
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        best.map(|i| syms[i].name.as_str())
    }

    /// `COIL_BACKTRACE=1`: explicit `panic` also prints the call stack
    /// (runtime errors always do). Off by default: checksum boards print
    /// their result with `panic` and compare the exact text.
    pub(crate) fn explicit_panic_backtrace(&self, panic_insn_ip: usize) -> Option<String> {
        let wanted = std::env::var("COIL_BACKTRACE").is_ok_and(|v| !v.is_empty() && v != "0");
        wanted.then(|| self.format_panic_backtrace(panic_insn_ip))
    }

    fn format_panic_backtrace(&self, panic_insn_ip: usize) -> String {
        let mut lines = Vec::new();
        // The top frame's line already names the panic site once its saved
        // ip is the panic instruction.
        let top_is_panic_site =
            !self.frames.is_empty() && self.frames.get().tell() == panic_insn_ip;
        if !top_is_panic_site && let Some(loc) = self.format_panic_location(panic_insn_ip) {
            lines.push(format!("  at {loc}"));
        }
        // Deep recursion repeats one frame line; print each run once.
        let mut repeats = 0usize;
        let flush = |lines: &mut Vec<String>, repeats: &mut usize| {
            if *repeats > 0 {
                lines.push(format!("  ... repeated {repeats} more times"));
                *repeats = 0;
            }
        };
        for frame_idx in (0..self.frames.len()).rev() {
            let ip = self.frames[frame_idx].tell();
            // The bootstrap frame (prologue `CALL main`) is not user code.
            if ip < 3 && self.fn_symbol_at_ip(ip).is_none() {
                continue;
            }
            let name = self.fn_symbol_at_ip(ip).unwrap_or("<unknown>");
            let line = match self.format_panic_location(ip) {
                Some(loc) => format!("  in {name} at {loc}"),
                None => format!("  in {name}"),
            };
            if lines.last() == Some(&line) {
                repeats += 1;
                continue;
            }
            flush(&mut lines, &mut repeats);
            lines.push(line);
        }
        flush(&mut lines, &mut repeats);
        lines.join("\n")
    }

    /// Abort execution with a VM panic (same path as `Instruction::Panic`).
    fn runtime_panic(&mut self, message: &str, panic_insn_ip: usize) -> bool {
        // The top frame's saved ip is stale mid-instruction; the backtrace
        // (and the debugger) read it.
        if !self.frames.is_empty() {
            self.frames.get_mut().seek(panic_insn_ip);
        }
        let loc_suffix = self
            .format_panic_location(panic_insn_ip)
            .map(|loc| format!(" at {loc}"))
            .unwrap_or_default();
        let backtrace = self.format_panic_backtrace(panic_insn_ip);
        if let Some(out) = self.output.as_mut() {
            let _ = write!(out, "panic: {message}{loc_suffix}");
            if !backtrace.is_empty() {
                let _ = write!(out, "\n{backtrace}");
            }
            let _ = out.flush();
        } else {
            eprint!("panic: {message}{loc_suffix}");
            if !backtrace.is_empty() {
                eprintln!("\n{backtrace}");
            }
            let _ = io::stderr().flush();
        }
        self.panicked = true;
        false
    }

    pub fn with_ffi_paths(mut self, base_dir: Option<PathBuf>, search_paths: Vec<PathBuf>) -> Self {
        self.set_ffi_paths(base_dir, search_paths);
        self
    }

    /// Replace every C struct layout (a reused worker VM takes its job's list).
    pub fn set_struct_layouts(&mut self, layouts: Arc<Vec<CStructLayout>>) {
        self.struct_layouts = layouts;
    }

    pub fn register_struct_layout(&mut self, layout: CStructLayout) -> u32 {
        let id = self.struct_layouts.len() as u32;
        Arc::make_mut(&mut self.struct_layouts).push(layout);
        id
    }

    /// Free function so `execute` can borrow `frames` and `heap` separately.
    /// Delegates to [`Heap::find_object_by_addr`] (mapped slot + header kind).
    fn find_object_by_addr(heap: &Heap, addr: u64) -> Option<Object> {
        heap.find_object_by_addr(addr)
    }

    /// `ObjEnum` at an exact slot. Heap-heap Result `Err` (`pointer | 1`) is
    /// not an enum cell, do not strip bit 0 here (GC marking already does).
    fn find_enum_exact(
        heap: &Heap,
        addr: u64,
    ) -> Option<crate::memory::Gc<crate::memory::ObjEnum>> {
        if addr & 1 != 0 {
            return None;
        }
        match Self::find_object_by_addr(heap, addr) {
            Some(Object::Enum(e)) => Some(e),
            _ => None,
        }
    }

    fn pop_call_frame(&mut self) -> usize {
        if unlikely(self.return_bookkeeping) {
            self.pop_pin_map_for_current_frame();
        }
        self.frames.pop().get()
    }

    /// Drop the pin table for the active frame if this frame created one.
    #[inline]
    fn pop_pin_map_for_current_frame(&mut self) {
        let depth = self.frames.len();
        if self.frame_pins.last().is_some_and(|p| p.depth == depth) {
            self.frame_pins.pop();
        }
    }

    #[inline]
    fn clear_dense_obj_cache(&mut self) {
        self.dense_obj_addr = 0;
        self.dense_obj = None;
    }

    #[inline]
    fn pinned_object(&self, slot: u32) -> Option<Object> {
        pinned_object_in(&self.frame_pins, self.frames.len(), slot)
    }

    /// Allocate a pin table only when this frame first pins an array.
    #[inline]
    fn pin_current_array(&mut self, slot: u32, obj: Object) {
        pin_current_array_in(&mut self.frame_pins, self.frames.len(), slot, obj);
        self.return_bookkeeping = true;
    }

    fn read_indexed(elements: &[Value], index: i64, unchecked: bool) -> Option<Value> {
        let len = elements.len();
        if unchecked {
            let idx = index as usize;
            promise!(index >= 0);
            promise!(idx < len);
            Some(unsafe { *elements.get_unchecked(idx) })
        } else if index >= 0 && (index as usize) < len {
            Some(unsafe { *elements.get_unchecked(index as usize) })
        } else {
            None
        }
    }

    fn vload(&mut self, vdest: usize, addr: u64, index: i64, _ty: u8) -> bool {
        let n = common::simd::LANES;
        let Some(Object::Array(gc)) = Self::find_object_by_addr(&self.heap, addr) else {
            return false;
        };
        let elems = &gc.as_ref().elements();
        if index < 0 || (index as usize).saturating_add(n) > elems.len() {
            return false;
        }
        let base = index as usize;
        for i in 0..n {
            self.vregs[vdest][i] = unsafe { elems.get_unchecked(base + i).raw() as u64 };
        }
        true
    }

    fn vstore(&mut self, vsrc: usize, addr: u64, index: i64, _ty: u8) -> bool {
        let n = common::simd::LANES;
        let Some(Object::Array(mut gc)) = Self::find_object_by_addr(&self.heap, addr) else {
            return false;
        };
        let arr = gc.as_mut();
        if index < 0 || (index as usize).saturating_add(n) > arr.len() {
            return false;
        }
        let base = index as usize;
        for i in 0..n {
            // SAFETY: `base + n <= len` checked above; SIMD lanes are
            // numeric (stride-1 numeric loops only), never references.
            unsafe { arr.set_numeric_unchecked(base + i, Value::from(self.vregs[vsrc][i])) };
        }
        true
    }

    fn ffi_type_from_value(v: &Value, heap: &Heap) -> crate::memory::FfiType {
        let (tag, aux) = Self::decode_ffi_type_tag(v, heap);
        crate::memory::FfiType::from_tag(tag, aux)
    }

    fn decode_ffi_type_tag(v: &Value, heap: &Heap) -> (u32, u32) {
        let raw = v.raw() as u64;
        if raw <= common::tag::STRUCT as u64 {
            return (raw as u32, 0);
        }
        if raw > 0xFFFF {
            return ((raw & 0xFFFF) as u32, (raw >> 16) as u32);
        }
        if let Some(crate::memory::Object::Enum(gc)) = Self::find_object_by_addr(heap, raw) {
            (gc.as_ref().tag, 0)
        } else {
            (common::tag::INT, 0)
        }
    }

    fn object_string_value(heap: &Heap, v: &Value) -> String {
        let addr = v.raw() as u64;
        let obj = Self::find_object_by_addr(heap, addr);
        if let Some(crate::memory::Object::String(gc)) = obj {
            gc.as_ref().data.clone()
        } else {
            String::new()
        }
    }

    fn intern_key(heap: &mut Heap, v: Value) -> crate::memory::RefString {
        if let Some(crate::memory::Object::String(gc)) =
            Self::find_object_by_addr(heap, v.raw() as u64)
        {
            return heap.intern_ref(gc);
        }
        heap.intern_str("")
    }

    /// Convert a runtime value to a display string (Show / `%v` / STRINGIFY).
    fn stringify_value(heap: &Heap, v: Value) -> String {
        let addr = v.raw() as u64;
        if v.raw().is_null() {
            // `Value::default()` / unit / false-ish null pointer.
            return "0".into();
        }
        match Self::find_object_by_addr(heap, addr) {
            Some(Object::Boxed(gc)) => {
                let b = gc.as_ref();
                match ValueTag::from_u16(b.tag) {
                    Some(ValueTag::Int) => match &b.payload {
                        Member::Value(iv) => iv.as_int().to_string(),
                        _ => "?".into(),
                    },
                    Some(ValueTag::Float) => match &b.payload {
                        Member::Value(iv) => format!("{:?}", iv.as_float()),
                        _ => "?".into(),
                    },
                    Some(ValueTag::Bool) => match &b.payload {
                        Member::Value(iv) => {
                            if iv.as_int() != 0 {
                                "true".into()
                            } else {
                                "false".into()
                            }
                        }
                        _ => "?".into(),
                    },
                    Some(ValueTag::String) => match &b.payload {
                        Member::Object(o) => {
                            Self::object_string_value(heap, &Value::from(o.addr()))
                        }
                        Member::Value(iv) => Self::object_string_value(heap, iv),
                    },
                    Some(ValueTag::Unit) => "()".into(),
                    _ => "?".into(),
                }
            }
            Some(Object::String(gc)) => gc.as_ref().data.clone(),
            Some(_) | None => v.as_int().to_string(),
        }
    }

    fn materialize_callback_args(
        &mut self,
        sig: &crate::ffi::FfiSignature,
        args: &[Value],
    ) -> Result<Vec<Value>, crate::ffi::FfiError> {
        use crate::ffi::{callback_cif, make_int_callback, VmCallFn};
        use crate::memory::FfiType;
        let mut out = args.to_vec();
        let vm_ptr = self as *mut Self as *mut c_void;
        let call_fn: VmCallFn = Self::invoke_call;
        for (i, ty) in sig.args.iter().enumerate() {
            if let FfiType::Callback(_) = ty {
                let offset = out[i].as_int() as u32;
                let cif = callback_cif(&[FfiType::Int], FfiType::Int, &self.struct_layouts)?;
                let closure = make_int_callback(vm_ptr, offset, call_fn, cif)?;
                let ptr = closure.code_ptr_usize();
                self.ffi_closures.push(closure);
                out[i] = Value::from(ptr as u64);
            }
        }
        Ok(out)
    }

    /// Register a new FFI function on the given library `Object`.
    fn register_signature_on_object(
        obj: &mut Object,
        sig: crate::ffi::FfiSignature,
        layouts: &[CStructLayout],
    ) -> Result<usize, crate::ffi::FfiError> {
        if let crate::memory::Object::Library(gc) = obj {
            let obj_lib: &mut crate::memory::ObjLibrary = (**gc).as_mut();
            crate::ffi::register_on_library(obj_lib, sig, layouts)
        } else {
            Err(crate::ffi::FfiError::InvalidHandle(
                "not a library object".into(),
            ))
        }
    }

    /// Load a shared library; returns its heap address as a `Value`.
    pub fn load_userland_library(&mut self, path: &str) -> Result<Value, String> {
        let lib_arc = crate::ffi::resolve_library(
            path,
            self.base_dir.as_deref(),
            &self.ffi_search_paths,
            &self.dload_gate,
        )
        .map_err(|e| e.to_string())?;
        let (object, _gc) = self.heap.alloc_library(lib_arc.clone());
        let addr = object.addr();
        self.userland_libraries.insert(addr, object);
        self.libraries
            .entry(path.to_string())
            .or_insert_with(|| lib_arc.clone());
        Ok(Value::from(addr as *mut u8))
    }

    /// Complete a GC cycle (explicit `gc::collect` / tests). Finishes any
    /// in-flight incremental sweep first so unmarked survivors are not freed.
    fn gc_collect(&mut self) {
        if self.heap.epoch_stw() {
            if let Some(e) = &self.shared_epoch {
                e.abort();
            }
            return;
        }
        if self.gc_in_progress {
            self.gc_deferred = true;
            return;
        }
        self.gc_in_progress = true;
        loop {
            self.gc_deferred = false;
            #[cfg(any(test, feature = "vm_profile"))]
            VM_GC_COUNT.with(|c| {
                c.fetch_add(1, Ordering::Relaxed);
            });

            if self.heap.gc_is_sweeping() {
                self.heap.finish_sweep();
            }
            self.clear_dense_obj_cache();

            self.mark_from_vm_roots();
            let queue = self.queue_unmarked_finalizers();
            if !queue.is_empty() {
                let mut gray = Vec::new();
                for (val, _) in &queue {
                    if let Some(obj) = Self::find_object_by_addr(&self.heap, val.raw() as u64) {
                        obj.mark(&mut gray);
                        obj.mark_references(&self.heap, &mut gray);
                    }
                }
                while let Some(obj) = gray.pop() {
                    obj.mark_references(&self.heap, &mut gray);
                }
                for (val, pc) in queue {
                    self.run_finalizer(val, pc);
                }
                self.unmark_heap();
                self.mark_from_vm_roots();
            }

            #[cfg(feature = "gc-stats")]
            self.note_gc_census();
            self.heap.clear_dead_weaks();
            // SAFETY: all reachable objects were marked above; dead weaks cleared.
            unsafe { self.heap.sweep() };
            // Cache is not a GC root; unmarked interned literals are gone.
            self.invalidate_program_string_cache();
            if !self.gc_deferred {
                break;
            }
        }
        self.gc_compact();
        self.gc_in_progress = false;
    }

    /// Evacuate sparse chunks after a finished cycle
    /// (`docs/internals/gc-evacuation.md`). Must-pointer frame slots are
    /// rewritten; every other VM root pins its target. Skipped while a host
    /// frame sits between VM frames (native re-entry, finalizers), in a
    /// shared-heap epoch, or under the debugger. `gc-stress` moves everything
    /// movable each cycle and checks that no word still names an old slot.
    #[cold]
    #[inline(never)]
    fn gc_compact(&mut self) {
        if !COMPACT_ENABLED.with(|e| *e)
            || IN_EXECUTE.get() > 1
            || self.shared_epoch.is_some()
            || self.heap.epoch_stw()
            || self.heap.is_borrowed()
            || !self.heap.gc_is_idle()
        {
            return;
        }
        #[cfg(any(test, feature = "debugger"))]
        if self.debug.is_some() {
            return;
        }
        // Outside stress mode, wait `compact_window` collections between
        // attempts; unproductive attempts double it (the analysis walks the
        // whole heap), a productive one resets it.
        let stress = cfg!(feature = "gc-stress");
        let (cycles, window) = COMPACT_PACING.get();
        if !stress && cycles + 1 < window {
            COMPACT_PACING.set((cycles + 1, window));
            return;
        }
        COMPACT_PACING.set((0, window));
        let mut pins = self.heap.take_gc_roots();
        let mut must = Vec::new();
        self.for_each_stack_word(&mut |idx, kind| match kind {
            crate::memory::RootKind::Precise => must.push(idx),
            _ => pins.push(self.stack[idx].heap_addr()),
        });
        let mut must_statics = Vec::new();
        self.for_each_static_root(&mut |i, addr, kind| match kind {
            crate::memory::RootKind::Precise => must_statics.push(i),
            _ => pins.push(addr),
        });
        self.for_each_handle_root(&mut |addr, _| pins.push(addr));
        let Some(plan) = self.heap.evacuate_plan(&pins, stress) else {
            self.heap.restore_gc_roots(pins);
            COMPACT_PACING.set((0, (window * 2).min(COMPACT_WINDOW_MAX)));
            return;
        };
        self.heap.restore_gc_roots(pins);
        for idx in must {
            if let Some(v) = plan.forward(self.stack[idx]) {
                self.stack[idx] = v;
            }
        }
        for i in must_statics {
            if let Some(v) = plan.forward(self.statics[i]) {
                self.statics[i] = v;
            }
        }
        #[cfg(feature = "gc-stress")]
        {
            self.heap.verify_evacuation(&plan);
            self.for_each_vm_root(&mut |addr, kind| {
                assert!(
                    !plan.moved().contains_key(&addr),
                    "gc-stress: {kind:?} VM root still names moved {addr:#x}"
                );
            });
        }
        let evacuation = self.heap.evacuate_finish(plan);
        self.clear_dense_obj_cache();
        COMPACT_PACING.set((0, if evacuation.capped { 1 } else { COMPACT_WINDOW }));
        #[cfg(feature = "gc-stats")]
        eprintln!("gc-stats evacuate: {evacuation:?}");
        let _ = evacuation;
    }

    /// `gc-stats`: census of the live set just marked, before weaks clear.
    #[cfg(feature = "gc-stats")]
    fn note_gc_census(&self) {
        let mut roots = Vec::new();
        self.for_each_vm_root(&mut |addr, kind| roots.push((addr, kind)));
        eprintln!("{}", self.heap.census(&roots));
    }

    fn gc_start_mark(&mut self) {
        // The dense-index cache is not a root (it kept the last indexed
        // array alive after its frame returned); drop it before objects can
        // be freed or moved under it.
        self.clear_dense_obj_cache();
        let roots = self.collect_vm_root_addrs();
        self.heap.begin_mark(&roots);
        self.heap.restore_gc_roots(roots);
    }

    fn gc_remark_vm_roots(&mut self) {
        let roots = self.collect_vm_root_addrs();
        self.heap.remark_roots(&roots);
        self.heap.restore_gc_roots(roots);
    }

    /// Mark to completion / lazy sweep at an alloc safepoint.
    #[inline(never)]
    fn gc_safepoint(&mut self) {
        if self.gc_in_progress {
            return;
        }
        if self.heap.epoch_stw() {
            if !self.heap.gc_is_idle() || self.heap.should_collect() {
                if let Some(e) = &self.shared_epoch {
                    e.abort();
                }
                if self.heap.is_borrowed() {
                    self.panicked = true;
                }
            }
            return;
        }
        match self.heap.gc_phase() {
            crate::memory::GcPhase::Idle => {
                if self.heap.should_collect() {
                    #[cfg(any(test, feature = "vm_profile"))]
                    VM_GC_COUNT.with(|c| {
                        c.fetch_add(1, Ordering::Relaxed);
                    });
                    self.gc_start_mark();
                    self.gc_mark_slice();
                }
            }
            crate::memory::GcPhase::Marking => self.gc_mark_slice(),
            crate::memory::GcPhase::Sweeping => self.gc_sweep_slice(),
        }
    }

    #[inline(never)]
    fn gc_mark_slice(&mut self) {
        // Drain mark at this safepoint: the mutator never runs with gray
        // objects (finalizers below run after the drain), so stores need no
        // write barrier. See `Heap::resurrect_during_mark`.
        while !self.heap.mark_quantum(usize::MAX) {}
        self.gc_remark_vm_roots();
        while !self.heap.mark_quantum(usize::MAX) {}
        let queue = self.queue_unmarked_finalizers();
        if !queue.is_empty() {
            for (val, _) in &queue {
                if let Some(obj) = Self::find_object_by_addr(&self.heap, val.raw() as u64) {
                    self.heap.shade_for_finalizer(obj);
                }
            }
            while !self.heap.mark_quantum(usize::MAX) {}
            self.gc_in_progress = true;
            for (val, pc) in queue {
                self.run_finalizer(val, pc);
            }
            self.gc_in_progress = false;
            self.gc_remark_vm_roots();
            while !self.heap.mark_quantum(usize::MAX) {}
        }
        #[cfg(feature = "gc-stats")]
        self.note_gc_census();
        self.heap.clear_dead_weaks();
        self.heap.begin_sweep();
        // Intern table is unlinked; unmarked literals will be freed across
        // later sweep quantums. Drop the STRING index cache now so the mutator
        // cannot reload a pointer that this cycle will dealloc (COI-410).
        self.invalidate_program_string_cache();
        self.gc_sweep_slice();
    }

    fn invalidate_program_string_cache(&mut self) {
        self.program_string_cache.fill(Value::default());
    }

    fn gc_sweep_slice(&mut self) {
        let n = self.heap.gc_sweep_quantum();
        if self.heap.sweep_quantum(n) {
            self.invalidate_program_string_cache();
            self.gc_compact();
        }
    }

    fn collect_vm_root_addrs(&mut self) -> Vec<u64> {
        let mut roots = self.heap.take_gc_roots();
        self.for_each_vm_root(&mut |addr, _| roots.push(addr));
        roots
    }

    /// Every VM root that resolves to a live heap object, tagged with how a
    /// moving collector could treat it (see `docs/internals/gc-evacuation.md`).
    /// Heap-owned roots (immortal unit enums, handle roots) are seeded by
    /// [`crate::memory::Heap::take_gc_roots`], not here.
    pub(crate) fn for_each_vm_root(&self, visit: &mut dyn FnMut(u64, crate::memory::RootKind)) {
        self.for_each_stack_root(visit);
        self.for_each_non_stack_root(visit);
    }

    /// VM roots outside the operand stack: typed statics, then
    /// [`Self::for_each_handle_root`].
    fn for_each_non_stack_root(&self, visit: &mut dyn FnMut(u64, crate::memory::RootKind)) {
        self.for_each_static_root(&mut |_, addr, kind| visit(addr, kind));
        self.for_each_handle_root(visit);
    }

    /// Static slots by their compile-time word kind (archive minor 29): a
    /// scalar static is no root, a pointer static is a precise root (a moving
    /// collector rewrites it), anything else stays ambiguous.
    fn for_each_static_root(&self, visit: &mut dyn FnMut(usize, u64, crate::memory::RootKind)) {
        let heap = &self.heap;
        let kinds = STATIC_KINDS.with_borrow(Arc::clone);
        for (i, v) in self.statics.iter().enumerate() {
            let kind = match kinds.get(i).copied().unwrap_or(common::WORD_UNKNOWN) {
                common::WORD_SCALAR => continue,
                common::WORD_POINTER => crate::memory::RootKind::Precise,
                _ => crate::memory::RootKind::Ambiguous,
            };
            let addr = v.heap_addr();
            let live = addr != 0 && heap.find_object_by_addr(addr).is_some();
            #[cfg(feature = "gc-stress")]
            assert!(
                addr == 0 || live || kind != crate::memory::RootKind::Precise,
                "gc-stress: pointer static {i} holds a non-object"
            );
            if live {
                visit(i, addr, kind);
            }
        }
    }

    /// VM roots outside the operand stack and statics. None of them can be
    /// rewritten by a moving collector (untyped words, or Rust-held handles).
    fn for_each_handle_root(&self, visit: &mut dyn FnMut(u64, crate::memory::RootKind)) {
        let heap = &self.heap;
        let word = |v: Value, kind: crate::memory::RootKind, visit: &mut dyn FnMut(u64, crate::memory::RootKind)| {
            let addr = v.heap_addr();
            if addr != 0 && heap.find_object_by_addr(addr).is_some() {
                visit(addr, kind);
            }
        };
        // Untyped word: the join result may be an immediate.
        word(self.steal_join_root, crate::memory::RootKind::Ambiguous, visit);
        for ctx in &self.resume_stack {
            visit(ctx.coro.as_ptr() as u64, crate::memory::RootKind::Precise);
        }
        for pins in &self.frame_pins {
            for obj in pins.by_slot.iter().flatten() {
                visit(obj.addr(), crate::memory::RootKind::Precise);
            }
        }
        // `FfiLoad` keeps `ObjLibrary` in `userland_libraries` for the VM
        // lifetime; the Coil handle is only an addr. Root those keys so GC
        // cannot sweep a live dload and `FfiInvoke` hit `invalid library handle`.
        // Keyed by address, so they must not move.
        for &addr in self.userland_libraries.keys() {
            visit(addr, crate::memory::RootKind::Pinned);
        }
        self.for_each_mapped_slot_index(|idx| {
            if idx < self.stack.capacity() {
                word(self.stack[idx], crate::memory::RootKind::Precise, visit);
            }
        });
    }

    /// Operand-stack roots, frame by frame: a frame with a trusted precise map
    /// roots only its heap slots; every other stack word is scanned.
    fn for_each_stack_root(&self, visit: &mut dyn FnMut(u64, crate::memory::RootKind)) {
        let heap = &self.heap;
        self.for_each_stack_word(&mut |idx, kind| {
            let v = self.stack[idx];
            let addr = v.heap_addr();
            #[cfg(feature = "gc-stress")]
            if kind == crate::memory::RootKind::Precise {
                assert!(
                    addr == 0 || heap.find_object_by_addr(addr).is_some(),
                    "gc-stress: must-pointer frame slot at stack word {idx} holds a non-object"
                );
            }
            if addr != 0 && heap.find_object_by_addr(addr).is_some() {
                visit(addr, kind);
            }
        });
    }

    /// Every operand-stack word that may root the heap, by stack index, frame
    /// by frame: a frame with a trusted precise map yields only its heap
    /// slots (must-pointer ones as `Precise`, the rest `Ambiguous`); every
    /// other word is scanned (`Ambiguous`).
    fn for_each_stack_word(&self, visit: &mut dyn FnMut(usize, crate::memory::RootKind)) {
        let scanned = crate::memory::RootKind::Ambiguous;
        let top = self.stack.as_slice().len();
        let n = self.frames.len();
        if self.precise_frames.is_empty() || n == 0 {
            (0..top).for_each(|idx| visit(idx, scanned));
            return;
        }
        (0..self.frames[0].get().min(top)).for_each(|idx| visit(idx, scanned));
        for i in 0..n {
            let lo = self.frames[i].get();
            let hi = if i + 1 < n { self.frames[i + 1].get() } else { top };
            if lo > hi || hi > top {
                // Unexpected frame layout: everything from here up is scanned.
                (lo.min(top)..top).for_each(|idx| visit(idx, scanned));
                return;
            }
            match self.trusted_precise_slots(i, lo, hi) {
                Some(slots) => {
                    // The top frame's stored locals may sit above the cursor.
                    let limit = if i + 1 == n { self.stack.capacity() } else { hi };
                    for &s in slots {
                        let idx = lo + common::precise_slot_index(s);
                        if idx < limit {
                            // Listed slots may hold heap words; only those
                            // flagged must-pointer are safe to rewrite.
                            let kind = if common::precise_slot_must(s) {
                                crate::memory::RootKind::Precise
                            } else {
                                crate::memory::RootKind::Ambiguous
                            };
                            visit(idx, kind);
                        }
                    }
                }
                None => {
                    // Slots stored past the cursor: cover the body's extent.
                    let reach = self
                        .frame_extent(i)
                        .map_or(hi, |words| lo.saturating_add(words).max(hi))
                        .min(self.stack.capacity());
                    (lo..reach).for_each(|idx| visit(idx, scanned));
                }
            }
        }
    }

    /// Heap slots of frame `i` (stack region `[lo, hi)`) from its precise
    /// map, when its PC is known: the top frame's safepoint PC, or a return
    /// address that follows a `CALL` / `CallIndirect`. Frames holding a coroutine segment or
    /// that re-entered the VM through native code (stale return PC) stay
    /// conservative.
    fn trusted_precise_slots(&self, i: usize, lo: usize, hi: usize) -> Option<&[u16]> {
        // A coroutine segment starting above this frame's base belongs to
        // frames this map does not describe.
        if !self.resume_stack.is_empty()
            && self.resume_stack.iter().any(|c| c.base_sp > lo && c.base_sp <= hi)
        {
            return None;
        }
        if !self.nested_frame_depths.is_empty()
            && self.nested_frame_depths.iter().any(|&d| d >= 2 && d - 2 == i)
        {
            return None;
        }
        // PCs are one past the op; look up the op itself so a body's last op
        // never resolves to the next body.
        let pc = if i + 1 == self.frames.len() {
            self.gc_top_ip?.checked_sub(1)?
        } else {
            let call_pc = self.frames[i].tell().checked_sub(1)?;
            if !matches!(
                self.instruction_at(call_pc)?,
                Instruction::CALL | Instruction::CallIndirect
            ) {
                return None;
            }
            call_pc
        };
        let pc = u32::try_from(pc).ok()?;
        common::precise_map_for_pc(&self.precise_frames, pc)?.slots_at_pc(pc)
    }

    /// Frame words of the body frame `i` runs (its PC need not be trusted:
    /// the extent only widens a conservative scan). `u32::MAX` is unbounded.
    fn frame_extent(&self, i: usize) -> Option<usize> {
        let ip = if i + 1 == self.frames.len() {
            self.gc_top_ip?
        } else {
            self.frames[i].tell()
        };
        let pc = u32::try_from(ip.checked_sub(1)?).ok()?;
        let words = common::precise_map_for_pc(&self.precise_frames, pc)?.frame_words;
        (words != 0).then_some(words as usize)
    }

    fn instruction_at(&self, pc: usize) -> Option<Instruction> {
        let code: &[Byte] = unsafe {
            std::slice::from_raw_parts(self.program_code.as_ptr().cast(), self.program_code.len())
        };
        code.get(pc).map(|b| *b.bytecode())
    }

    fn for_each_mapped_slot_index(&self, mut visit: impl FnMut(usize)) {
        if self.stack_maps.is_empty() {
            return;
        }
        let n = self.frames.len();
        for i in 0..n {
            let sp = self.frames[i].get();
            let ip = if i + 1 == n {
                self.gc_ip as u32
            } else {
                self.frames[i].tell() as u32
            };
            let Some(map) = common::map_for_ip(&self.stack_maps, ip) else {
                continue;
            };
            for &slot in map.slots_at(ip) {
                visit(sp.saturating_add(slot as usize));
            }
        }
    }

    #[cfg(any(test, feature = "debugger"))]
    pub fn stack_at_for_test(&self, idx: usize) -> Value {
        self.stack[idx]
    }

    /// Test helper: apply `rewrite` to every mapped heap slot.
    #[cfg(any(test, feature = "debugger"))]
    pub fn rewrite_mapped_slots_for_test(&mut self, rewrite: impl Fn(u64) -> u64) {
        if self.gc_ip == 0 {
            self.gc_ip = self.frames.get().tell();
        }
        let mut idxs = Vec::new();
        self.for_each_mapped_slot_index(|i| idxs.push(i));
        for idx in idxs {
            if idx >= self.stack.capacity() {
                continue;
            }
            let addr = self.stack[idx].heap_addr();
            if addr == 0 {
                continue;
            }
            let next = rewrite(addr);
            if next != addr {
                self.stack[idx] = Value::from(next);
            }
        }
    }

    fn mark_from_vm_roots(&mut self) {
        let roots = self.collect_vm_root_addrs();
        self.heap.mark_from_roots(&roots);
        self.heap.restore_gc_roots(roots);
    }

    fn unmark_heap(&self) {
        for obj in self.heap.objects() {
            obj.unmark();
        }
    }

    fn queue_unmarked_finalizers(&self) -> Vec<(Value, u32)> {
        if self.finalizer_by_type.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for obj in self.heap.objects() {
            if !obj.is_marked()
                && let Some(pc) = self.pending_finalizer(obj)
            {
                out.push((Value::from(obj.addr()), pc));
            }
        }
        out
    }

    /// Drop PC for a tagged instance or enum whose finalizer has not run.
    fn pending_finalizer(&self, obj: Object) -> Option<u32> {
        let (type_id, finalized) = match obj {
            Object::Instance(gc) => (gc.as_ref().type_id, gc.as_ref().finalized),
            Object::Enum(gc) => (gc.as_ref().type_id, gc.as_ref().finalized),
            _ => return None,
        };
        if type_id == 0 || finalized {
            return None;
        }
        self.finalizer_by_type.get(&type_id).copied()
    }

    fn claim_finalizer(&self, v: Value) -> bool {
        match Self::find_object_by_addr(&self.heap, v.raw() as u64) {
            Some(Object::Instance(gc)) => {
                let inst = gc.payload_mut();
                !std::mem::replace(&mut inst.finalized, true)
            }
            // Untagged enums (unit variants: shared immortals) are never
            // finalized, so an explicit `drop()` on one just runs the body.
            Some(Object::Enum(gc)) => {
                let e = gc.payload_mut();
                e.type_id == 0 || !std::mem::replace(&mut e.finalized, true)
            }
            _ => false,
        }
    }

    fn run_finalizer(&mut self, self_val: Value, pc: u32) {
        if !self.claim_finalizer(self_val) {
            return;
        }
        let was_panicked = self.panicked;
        self.panicked = false;
        let _ = self.call_function(pc, &[self_val]);
        self.panicked = was_panicked;
    }

    fn register_finalizer(&mut self, type_id: u32, pc: u32) {
        if type_id == 0 {
            return;
        }
        self.finalizer_by_type.insert(type_id, pc);
        self.finalizer_pcs.insert(pc);
    }

    fn run_remaining_finalizers(&mut self) {
        if self.program_code.is_empty() || self.finalizer_by_type.is_empty() {
            return;
        }
        let mut queue = Vec::new();
        for obj in self.heap.objects() {
            if let Some(pc) = self.pending_finalizer(obj) {
                queue.push((Value::from(obj.addr()), pc));
            }
        }
        for (val, pc) in queue {
            self.run_finalizer(val, pc);
        }
    }

    /// In-place `Vec` / array grow. Returns false when `target` is not an array.
    fn array_push_value(&mut self, target: Value, value: Value) -> bool {
        let target_addr = target.raw() as u64;
        let Some(crate::memory::Object::Array(mut gc)) =
            Self::find_object_by_addr(&self.heap, target_addr)
        else {
            return false;
        };
        let old_bytes = gc.as_ref().elements().capacity() * std::mem::size_of::<Value>();
        gc.as_mut().push(value);
        let new_bytes = gc.as_ref().elements().capacity() * std::mem::size_of::<Value>();
        if old_bytes != new_bytes {
            self.heap.account_resize(old_bytes, new_bytes);
        }
        true
    }

    /// Incremental GC work after an allocation safepoint.
    #[inline]
    fn maybe_gc_after_alloc(&mut self, ip: usize) {
        if likely(self.heap.gc_is_idle() && !self.heap.should_collect()) {
            return;
        }
        self.gc_safepoint_from_alloc(ip);
    }

    #[inline(never)]
    fn gc_safepoint_from_alloc(&mut self, ip: usize) {
        if unlikely(!self.stack_maps.is_empty()) {
            self.gc_ip = ip;
        }
        self.gc_top_ip = Some(ip);
        self.gc_safepoint();
        self.gc_top_ip = None;
    }

    /// Classify a stack value as an enum member; heap pointers become `Object`.
    #[inline]
    fn value_as_member(heap: &Heap, v: Value) -> Member {
        let addr = v.raw() as u64;
        if let Some(o) = Self::find_object_by_addr(heap, addr) {
            Member::Object(o)
        } else {
            Member::Value(v)
        }
    }

    /// `MakeEnum` packing: tag in `[31:16]`, arity in `[15:0]`.
    /// Payloads stay on the stack through alloc so GC can root them; the
    /// fresh object is pushed before `maybe_gc_after_alloc`.
    fn push_make_enum(&mut self, opcode: &Byte, ip: usize) {
        let tag = opcode.operand_u32() >> 16;
        let arity = opcode.make_arity() as usize;
        if arity == 0 {
            let object = self.heap.immortal_unit_enum(tag);
            self.stack.push(Value::from(object.addr()));
            return;
        }
        let sp = self.stack.tell();
        promise!(sp >= arity);
        if arity <= 3 {
            note_make_fast();
        }
        let payload =
            Self::stack_copy_enum_payload(&self.stack, sp, arity).with_kinds(opcode.make_kinds());
        let obj_enum = ObjEnum::new(tag, payload);
        let (object, _) = self.heap.alloc(obj_enum, Object::Enum);
        self.stack.seek(sp - arity);
        self.stack.push(Value::from(object.addr()));
        self.maybe_gc_after_alloc(ip);
    }

    /// Copy `n` stack values in declaration order (`stack[base..base+n]`).
    /// Used by MakeTuple / MakeArray. Args stay on the stack for GC rooting
    /// until the caller seeks past them after allocation.
    #[inline]
    fn stack_copy_decl(stack: &Stack<Value>, base: usize, n: usize) -> Vec<Value> {
        match n {
            0 => Vec::new(),
            1 => vec![stack[base]],
            2 => vec![stack[base], stack[base + 1]],
            3 => vec![stack[base], stack[base + 1], stack[base + 2]],
            _ => {
                let mut values = Vec::with_capacity(n);
                for i in 0..n {
                    values.push(stack[base + i]);
                }
                values
            }
        }
    }

    /// Copy `n` stack values in MakeEnum pop order (TOS → payload[0]).
    /// Codegen reverse-pushes constructor args so this yields declaration order.
    /// Arity ≤ [`crate::ENUM_INLINE_ARITY`] stays off the Rust global allocator.
    #[inline]
    fn stack_copy_enum_payload(stack: &Stack<Value>, sp: usize, n: usize) -> EnumPayload {
        match n {
            0 => EnumPayload::empty(),
            1 => EnumPayload::one(stack[sp - 1]),
            2 => EnumPayload::two(stack[sp - 1], stack[sp - 2]),
            _ => EnumPayload::from_vec((0..n).map(|i| stack[sp - 1 - i]).collect()),
        }
    }

    /// Declaration-order slots (DenseMake), not TOS-first stack pops.
    #[inline]
    fn dense_enum_payload(values: &[Value]) -> EnumPayload {
        match values.len() {
            0 => EnumPayload::empty(),
            1 => EnumPayload::one(values[0]),
            2 => EnumPayload::two(values[0], values[1]),
            _ => EnumPayload::from_slice(values),
        }
    }

    fn saved_stack_live_mask(heap: &Heap, values: &[Value]) -> u64 {
        let mut mask = 0u64;
        for (i, v) in values.iter().enumerate() {
            if i >= 64 {
                break;
            }
            let addr = v.heap_addr();
            if addr != 0 && heap.find_object_by_addr(addr).is_some() {
                mask |= 1u64 << i;
            }
        }
        mask
    }

    /// Intern `data`, push the GC pointer, then maybe collect.
    ///
    /// The intern table is a cache, not a GC root, unmarked interned strings
    /// are swept. The new object must be on the operand stack before
    /// [`Self::gc_collect`] so it survives the cycle.
    fn push_interned_string(&mut self, data: String, ip: usize) {
        let gc_string = self.heap.intern(data);
        self.stack
            .push(Value::from(gc_string.as_ptr() as *mut u8 as u64));
        self.maybe_gc_after_alloc(ip);
    }

    fn install_program_strings(&mut self, strings: &[String]) {
        self.install_program_strings_arc(Arc::new(strings.to_vec()));
    }

    fn install_program_strings_arc(&mut self, strings: Arc<Vec<String>>) {
        self.program_strings = strings;
        self.program_string_cache.clear();
        self.program_string_cache
            .resize(self.program_strings.len(), Value::default());
    }

    fn push_program_string(&mut self, idx: usize, ip: usize) {
        let cached = unsafe { *self.program_string_cache.get_unchecked(idx) };
        if likely(!cached.raw().is_null()) {
            self.stack.push(cached);
            return;
        }
        let data = unsafe { self.program_strings.get_unchecked(idx) };
        let gc_string = self.heap.intern_str(data);
        let handle = Value::from(gc_string.as_ptr() as *mut u8 as u64);
        self.stack.push(handle);
        self.maybe_gc_after_alloc(ip);
        // Re-store after maybe-GC: sweep zeros the cache (not a root).
        unsafe {
            *self.program_string_cache.get_unchecked_mut(idx) = handle;
        }
    }
}

impl<const S: usize> Machine<S> {
    #[cfg(test)]
    pub fn push(&mut self, value: Value) {
        self.stack.push(value);
    }

    #[cfg(test)]
    pub fn pop(&mut self) -> Value {
        self.stack.pop()
    }

    #[cfg(test)]
    pub fn tell(&self) -> usize {
        self.stack.tell()
    }

    /// Redirect `PRINT` output (used by pipeline tests).
    pub fn with_output<W: IoWrite + Send + 'static>(&mut self, writer: W) -> Option<OutputSink> {
        let prev = self.output.take();
        self.output = Some(Box::new(writer));
        if let Some(out) = self.output.as_mut() {
            crate::io::set_output_redirect(Some(out.as_mut() as *mut (dyn IoWrite + Send)));
        }
        prev
    }

    /// Reset the output sink back to stdout. Returns the previous
    /// sink so the caller can recover it (useful in tests that
    /// want to scope the redirection).
    pub fn restore_output(&mut self) -> Option<OutputSink> {
        crate::io::set_output_redirect(None);
        self.output.take()
    }

    /// Register a host native with an explicit signature via the
    /// builder API. Returns the stable native id used by
    /// [`Instruction::HostInvoke`].
    pub fn register_fn<F>(&mut self, sig: crate::ffi::FfiSignature, func: F) -> usize
    where
        F: Fn(&mut Heap, &[Value]) -> Result<Option<Value>, crate::ffi::FfiError>
            + Send
            + Sync
            + 'static,
    {
        self.natives
            .register(std::sync::Arc::new(crate::ffi::HostClosureFn::new(
                sig, func,
            )))
    }

    /// Back-compat alias for [`Self::register_fn`].
    pub fn register_native(&mut self, native: std::sync::Arc<dyn crate::ffi::NativeFn>) -> usize {
        self.natives.register(native)
    }

    /// Replace the host-native table with a clone of `other` (worker threads).
    pub fn install_natives(&mut self, other: &crate::ffi::Natives) {
        self.natives = other.clone_registry();
    }

    pub fn set_thread_program(&mut self, program: std::sync::Arc<crate::thread::ThreadProgram>) {
        self.stack_maps = program.stack_maps.clone();
        self.precise_frames = Arc::clone(&program.precise_frames);
        self.heap
            .set_class_word_kinds(Arc::clone(&program.class_word_kinds));
        STATIC_KINDS.with_borrow_mut(|k| *k = Arc::clone(&program.static_word_kinds));
        self.thread_program = Some(program);
    }

    /// Attach S2b maps (compile-and-run / archive load). Empty keeps conservative stack GC.
    pub fn set_stack_maps(&mut self, maps: Vec<common::FrameStackMap>) {
        self.stack_maps = maps;
    }

    pub fn stack_maps(&self) -> &[common::FrameStackMap] {
        &self.stack_maps
    }

    /// Attach complete frame maps. Empty keeps every frame conservative.
    pub fn set_precise_frames(&mut self, maps: Arc<Vec<common::PreciseFrameMap>>) {
        self.precise_frames = maps;
    }

    pub fn thread_program(&self) -> Option<&crate::thread::ThreadProgram> {
        self.thread_program.as_deref()
    }

    pub fn set_shared_print(&mut self, buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>) {
        self.shared_print = Some(buf.clone());
        crate::io::set_shared_print_redirect(Some(buf));
    }

    pub fn shared_print(&self) -> Option<std::sync::Arc<std::sync::Mutex<Vec<u8>>>> {
        self.shared_print.clone()
    }

    /// Replace the undetached-spawn registry (used by workers to share the
    /// root VM's list so nested `spawn` still joins with the root).
    pub fn set_live_threads(&mut self, registry: crate::thread::LiveThreadRegistry) {
        self.live_threads = registry;
    }

    pub fn live_threads(&self) -> &crate::thread::LiveThreadRegistry {
        &self.live_threads
    }

    /// Share the root VM's worker-thread budget with nested workers.
    pub fn set_worker_cap(&mut self, cap: std::sync::Arc<crate::thread::WorkerCap>) {
        self.worker_cap = cap;
    }

    pub fn worker_cap(&self) -> &std::sync::Arc<crate::thread::WorkerCap> {
        &self.worker_cap
    }

    /// Share the root VM's work-stealing reactor with nested workers.
    pub fn set_reactor(&mut self, reactor: std::sync::Arc<crate::reactor::Reactor>) {
        self.reactor = reactor;
    }

    pub fn reactor(&self) -> &std::sync::Arc<crate::reactor::Reactor> {
        &self.reactor
    }

    /// Share the root VM's IO reactor with nested workers.
    pub fn set_io_reactor(&mut self, io: std::sync::Arc<crate::io_reactor::IoReactor>) {
        self.io_reactor = io;
    }

    pub fn io_reactor(&self) -> &std::sync::Arc<crate::io_reactor::IoReactor> {
        &self.io_reactor
    }

    /// Allocate global static slots without running bytecode.
    pub fn init_static_slots(&mut self, static_slots: u32) {
        self.statics = vec![Value::default(); static_slots as usize];
    }

    pub fn heap_mut(&mut self) -> &mut Heap {
        self.heap.get_mut()
    }

    /// Snapshot needed to spawn a worker on this program.
    pub fn thread_spawn_context(&self) -> Option<crate::thread::ThreadSpawnContext> {
        let program = self.thread_program.clone()?;
        Some(crate::thread::ThreadSpawnContext {
            program,
            natives: self.natives.clone_registry(),
            shared_print: self.shared_print.clone(),
            live_threads: std::sync::Arc::clone(&self.live_threads),
            worker_cap: std::sync::Arc::clone(&self.worker_cap),
            reactor: std::sync::Arc::clone(&self.reactor),
            io_reactor: std::sync::Arc::clone(&self.io_reactor),
            ffi_base_dir: self.base_dir.clone(),
            ffi_search_paths: self.ffi_search_paths.clone(),
            dload_gate: self.dload_gate.clone(),
            struct_layouts: Arc::clone(&self.struct_layouts),
        })
    }

    fn sync_thread_program_from_current(&mut self) {
        if self.thread_program.is_some() {
            return;
        }
        if self.program_code.is_empty() {
            return;
        }
        self.thread_program = Some(std::sync::Arc::new(crate::thread::ThreadProgram {
            code: Arc::clone(&self.program_code),
            constants: Arc::clone(&self.program_constants),
            strings: Arc::clone(&self.program_strings),
            static_slot_count: self.statics.len() as u32,
            debug: self.program_debug.clone(),
            operand_stack_slots: self.stack.capacity() as u32,
            stack_maps: self.stack_maps.clone(),
            precise_frames: Arc::clone(&self.precise_frames),
            class_word_kinds: self.heap.class_word_kinds_table(),
            static_word_kinds: STATIC_KINDS.with_borrow(Arc::clone),
        }));
    }

    /// Register a function signature on a previously-loaded
    /// userland library (host/test helper, userland code uses
    /// `DeclareFFI` at runtime).
    pub fn register_ffi_function(
        &mut self,
        library_value: Value,
        signature: crate::ffi::FfiSignature,
    ) -> Result<usize, String> {
        let addr = library_value.raw() as u64;
        let mut lib_obj = self
            .userland_libraries
            .get(&addr)
            .copied()
            .ok_or_else(|| format!("not a loaded library: 0x{:x}", addr))?;
        if let crate::memory::Object::Library(gc) = &mut lib_obj {
            let obj_lib: &mut crate::memory::ObjLibrary = (**gc).as_mut();
            let id = crate::ffi::register_on_library(obj_lib, signature, &self.struct_layouts)
                .map_err(|e| e.to_string())?;
            self.userland_libraries.insert(addr, lib_obj);
            Ok(id)
        } else {
            Err("not a library object".to_string())
        }
    }

    /// Manually trigger GC (for tests).
    pub fn collect_garbage(&mut self) {
        self.gc_collect();
    }

    #[cfg(test)]
    pub fn finalizer_pc(&self, type_id: u32) -> Option<u32> {
        self.finalizer_by_type.get(&type_id).copied()
    }

    #[cfg(test)]
    pub fn instance_meta(&self, v: Value) -> Option<(u32, bool)> {
        match Self::find_object_by_addr(&self.heap, v.raw() as u64) {
            Some(Object::Instance(gc)) => {
                let inst = gc.as_ref();
                Some((inst.type_id, inst.finalized))
            }
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn register_finalizer_for_test(&mut self, type_id: u32, pc: u32) {
        self.register_finalizer(type_id, pc);
    }

    #[cfg(test)]
    pub fn live_pin_map_count(&self) -> usize {
        self.frame_pins.len()
    }

    #[cfg(test)]
    pub fn pinned_addr_for_test(&self, slot: u32) -> Option<u64> {
        self.pinned_object(slot).map(|obj| obj.addr())
    }

    #[cfg(test)]
    pub fn dense_cache_addr_for_test(&self) -> u64 {
        self.dense_obj_addr
    }

    fn with_coroutine_mut(coro: RefCoroutine, f: impl FnOnce(&mut ObjCoroutine)) {
        f(coro.payload_mut());
    }

    /// Parent delegating to `sub` via `yield from`, if it still is.
    fn find_delegator(&self, sub: RefCoroutine) -> Option<RefCoroutine> {
        sub.as_ref().delegator.filter(|parent| {
            parent
                .as_ref()
                .yield_from
                .is_some_and(|d| d.as_ptr() == sub.as_ptr())
        })
    }

    fn save_coroutine_state(
        &self,
        coro_gc: RefCoroutine,
        ip: usize,
        sp: usize,
        base_sp: usize,
        frame_depth: usize,
    ) {
        let top = self.stack.tell();
        let segment = if base_sp <= top {
            self.stack.as_slice()[base_sp..top].to_vec()
        } else {
            Vec::new()
        };
        let current_depth = self.frames.len();
        let mut saved_frames = Vec::new();
        for idx in (frame_depth + 1)..current_depth {
            saved_frames.push((
                self.frames[idx].tell(),
                self.frames[idx].get().saturating_sub(base_sp),
            ));
        }
        if saved_frames.is_empty() {
            saved_frames.push((ip, sp.saturating_sub(base_sp)));
        } else {
            saved_frames.last_mut().unwrap().0 = ip;
        }

        let live_mask = Self::saved_stack_live_mask(&self.heap, &segment);
        Self::with_coroutine_mut(coro_gc, |coro| {
            coro.saved_stack = segment;
            coro.saved_live_mask = live_mask;
            coro.saved_frames = saved_frames;
            coro.resume_ip = ip;
            coro.state = CoroState::Suspended;
        });
    }

    fn after_return(&mut self, ip: &mut usize, sp: &mut usize) {
        let caller = self.frames.get_mut();
        *ip = caller.tell();
        *sp = caller.get();
        // Coroutine resume bookkeeping is cold for ordinary calls (fib).
        if unlikely(self.return_bookkeeping)
            && !self.resume_stack.is_empty()
            && let Some(ctx) = self.resume_stack.last()
            && self.frames.len() <= ctx.frame_depth
        {
            let coro_ref = ctx.coro;
            let old_wait = {
                let mut taken = None;
                Self::with_coroutine_mut(coro_ref, |coro| {
                    // Outer coroutines suspended via `yield from` stay on
                    // `resume_stack` while main runs; host RETURN must not
                    // treat that as coroutine completion.
                    if coro.yield_from.is_some() {
                        return;
                    }
                    taken = coro.io_wait.take();
                    coro.state = CoroState::Done;
                    coro.saved_stack.clear();
                    coro.saved_frames.clear();
                    coro.yield_from = None;
                });
                taken
            };
            if let Some(tok) = old_wait {
                self.io_reactor.cancel_wait(tok);
            }
            self.resume_stack.pop();
        }
        if unlikely(self.return_bookkeeping) {
            self.return_bookkeeping = self.nested_depth > 0
                || !self.resume_stack.is_empty()
                || !self.frame_pins.is_empty();
        }
    }

    /// Register handle interest and yield so other coros / `wait_ready` can batch.
    ///
    /// Pushes `Ok(())` onto the coroutine stack before yielding so resume
    /// continues after `HostInvoke` as if the await completed. Callers must
    /// `wait_ready` (or tolerate L0 `WouldBlock`) before the next resume.
    fn cooperative_io_await_yield(
        &mut self,
        ip: &mut usize,
        sp: &mut usize,
        req: crate::io::IoParkRequest,
        layout: crate::host_enum::HostEnumLayout,
    ) {
        let token = self.io_reactor.register_wait(req.handle, req.interest);
        let coro_ref = self
            .resume_stack
            .last()
            .expect("cooperative await requires an active coroutine")
            .coro;
        let old = {
            let mut taken = None;
            Self::with_coroutine_mut(coro_ref, |c| {
                taken = c.io_wait.replace(token);
            });
            taken
        };
        if let Some(old) = old {
            self.io_reactor.cancel_wait(old);
        }
        let ok = crate::host_enum::with_host_enum_layout(layout, || {
            crate::io::as_result_unit(&mut self.heap, Ok(()))
        });
        self.stack.push(ok);
        // Yield value is discarded by `block_on`; multiplex loops ignore it.
        self.yield_coroutine(ip, sp, Value::from(0_i64));
    }

    fn resume_coroutine(
        &mut self,
        ip: &mut usize,
        sp: &mut usize,
        gc: RefCoroutine,
        send_val: Value,
        code: &[Byte],
        push_send_for_receive: bool,
    ) -> bool {
        // The saved segment (and a sent value) land above the cursor.
        if !self.reserve_operand_words(gc.as_ref().saved_stack.len() + 1) {
            return false;
        }
        let return_ip = *ip;
        let coro = gc.as_ref();
        let base_sp = self.stack.tell();
        let base_depth = self.frames.len();

        self.frames.get_mut().seek(return_ip);

        let old_wait = {
            let mut taken = None;
            Self::with_coroutine_mut(gc, |c| {
                taken = c.io_wait.take();
                c.pending_send = send_val;
            });
            taken
        };
        if let Some(tok) = old_wait {
            self.io_reactor.cancel_wait(tok);
        }

        self.return_bookkeeping = true;
        self.resume_stack.push(ResumeCtx {
            coro: gc,
            base_sp,
            frame_depth: self.frames.len(),
        });

        for v in &coro.saved_stack {
            self.stack.push(*v);
        }

        for &(frame_ip, sp_off) in &coro.saved_frames {
            self.frames.setup_current_and_advance(|f| {
                f.seek(frame_ip);
                f.set(base_sp + sp_off);
            });
            // Pins are not saved across yield; ArrayPin after resume allocates.
        }

        *ip = coro.resume_ip;
        *sp = base_sp + coro.saved_frames.last().map_or(0, |(_, off)| *off);

        if push_send_for_receive
            && *ip < code.len()
            && matches!(
                code[*ip].bytecode(),
                Instruction::STORE | Instruction::StorePop
            )
        {
            self.stack.push(send_val);
        }
        self.keep_call_window(base_depth)
    }

    fn delegate_yield_to_parent(
        &mut self,
        sub_gc: RefCoroutine,
        ip: &mut usize,
        sp: &mut usize,
        yield_val: Value,
        sub_base_sp: usize,
        sub_frame_depth: usize,
    ) {
        let Some(parent) = self.find_delegator(sub_gc) else {
            return;
        };

        self.save_coroutine_state(sub_gc, *ip, *sp, sub_base_sp, sub_frame_depth);

        let parent_entry_idx = self
            .resume_stack
            .iter()
            .position(|c| c.coro.as_ptr() == parent.as_ptr())
            .unwrap_or(self.resume_stack.len().saturating_sub(1));
        let parent_ctx = &self.resume_stack[parent_entry_idx];
        let parent_base_sp = parent_ctx.base_sp;
        let parent_frame_depth = parent_ctx.frame_depth;

        self.save_coroutine_state(
            parent,
            parent.as_ref().yield_from_resume_ip,
            self.stack.tell(),
            parent_base_sp,
            parent_frame_depth,
        );

        self.stack.seek(parent_base_sp);
        while self.frames.len() > parent_frame_depth {
            self.pop_pin_map_for_current_frame();
            self.frames.pop();
        }
        if self.resume_stack.len() > parent_entry_idx + 1 {
            self.resume_stack.truncate(parent_entry_idx + 1);
        }

        self.stack.push(yield_val);
        let caller = self.frames.get_mut();
        *ip = caller.tell();
        *sp = caller.get();
        // Mirror `yield_coroutine`: delegating coroutine is not active while
        // main runs between resumes.
        self.resume_stack.pop();
    }

    fn yield_coroutine(&mut self, ip: &mut usize, sp: &mut usize, yield_val: Value) {
        let Some(ctx) = self
            .resume_stack
            .last()
            .map(|c| (c.coro, c.base_sp, c.frame_depth))
        else {
            self.stack.push(yield_val);
            return;
        };
        let (coro_gc, base_sp, frame_depth) = ctx;

        if self.find_delegator(coro_gc).is_some() {
            self.delegate_yield_to_parent(coro_gc, ip, sp, yield_val, base_sp, frame_depth);
            return;
        }

        let current_depth = self.frames.len();
        let top = self.stack.tell();
        let coro_sp = if current_depth > frame_depth {
            self.frames[current_depth - 1].get()
        } else {
            base_sp
        };
        let segment = if coro_sp <= top {
            self.stack.as_slice()[coro_sp..top].to_vec()
        } else {
            Vec::new()
        };
        let mut saved_frames = Vec::new();
        for idx in (frame_depth + 1)..current_depth {
            saved_frames.push((self.frames[idx].tell(), self.frames[idx].get() - base_sp));
        }
        if saved_frames.is_empty() {
            saved_frames.push((*ip, *sp - base_sp));
        } else {
            saved_frames.last_mut().unwrap().0 = *ip;
        }

        let live_mask = Self::saved_stack_live_mask(&self.heap, &segment);
        Self::with_coroutine_mut(coro_gc, |coro| {
            coro.saved_stack = segment;
            coro.saved_live_mask = live_mask;
            coro.saved_frames = saved_frames;
            coro.resume_ip = *ip;
            coro.state = CoroState::Suspended;
        });

        self.stack.seek(base_sp);
        while self.frames.len() > frame_depth {
            self.pop_pin_map_for_current_frame();
            self.frames.pop();
        }

        self.stack.push(yield_val);
        let caller = self.frames.get_mut();
        *ip = caller.tell();
        *sp = caller.get();
        self.resume_stack.pop();
    }

    fn start_yield_from(
        &mut self,
        ip: &mut usize,
        sp: &mut usize,
        sub: RefCoroutine,
        code: &[Byte],
    ) -> bool {
        let Some(outer_ctx) = self.resume_stack.last().copied() else {
            return true;
        };
        let outer = outer_ctx.coro;
        self.save_coroutine_state(outer, *ip, *sp, outer_ctx.base_sp, outer_ctx.frame_depth);
        Self::with_coroutine_mut(outer, |outer_coro| {
            outer_coro.yield_from = Some(sub);
            outer_coro.yield_from_resume_ip = *ip;
        });
        Self::with_coroutine_mut(sub, |sub_coro| sub_coro.delegator = Some(outer));
        self.resume_coroutine(ip, sp, sub, Value::from(0_i64), code, false)
    }

    /// Read-only access to the heap. Used by the GC integration
    /// test to assert that the heap didn't grow unboundedly.
    pub fn heap(&self) -> &Heap {
        self.heap.get()
    }

    /// True when a language-level `panic` aborted the last run.
    pub fn panicked(&self) -> bool {
        self.panicked
    }

    /// Load bytecode for reentrant [`call_function`] without running `main`.
    pub fn load_program(&mut self, code: &[RawByte], constants: &[u64], strings: &[String]) {
        self.program_code = Arc::new(code.to_vec());
        self.program_constants = Arc::new(constants.to_vec());
        self.install_program_strings(strings);
        self.panicked = false;
    }

    /// Pin an already-shared program image (reactor workers / join-help).
    ///
    /// Does not memcpy bytecode, constants, or the string table.
    pub fn load_shared_program(
        &mut self,
        code: Arc<Vec<RawByte>>,
        constants: Arc<Vec<u64>>,
        strings: Arc<Vec<String>>,
    ) {
        self.program_code = code;
        self.program_constants = constants;
        self.install_program_strings_arc(strings);
        self.panicked = false;
    }

    /// Drop isolate GC identity after a reactor job. Operand-stack capacity
    /// is kept. More than one mapped slab chunk is unmapped so RSS cannot
    /// climb with successive jobs; a single 64KiB chunk is collected in place.
    pub fn reset_isolate_heap(&mut self) {
        self.frames = {
            let mut frames = ArrayVec::default();
            frames.consume();
            frames
        };
        self.frame_pins.clear();
        self.clear_dense_obj_cache();
        self.stack.seek(0);
        self.resume_stack.clear();
        self.statics.fill(Value::default());
        self.program_string_cache.fill(Value::default());
        self.nested_depth = 0;
        self.return_bookkeeping = false;
        self.nested_frame_depths.clear();
        self.nested_return = None;
        self.pending_ffi = None;
        self.pending_io = None;
        self.panicked = false;
        self.userland_libraries.clear();
        self.ffi_closures.clear();
        // Drop PCs belong to the job's program: a reused worker may load a
        // different one next, where the same `type_id` names another type.
        self.finalizer_by_type.clear();
        self.finalizer_pcs.clear();
        self.gc_in_progress = false;
        self.gc_deferred = false;
        if self.heap.is_borrowed() {
            return;
        }
        if self.heap.owned_mut().slab_chunk_count() > 1 {
            *self.heap.owned_mut() = Heap::default();
        } else if self.heap.owned_mut().slab_chunk_count() == 1 {
            self.heap.owned_mut().collect(&[]);
            self.program_string_cache.fill(Value::default());
        }
    }

    /// Drop frames / pins after a C1/C2 shared-heap job. Does not unmap the
    /// borrowed epoch Heap.
    pub fn reset_shared_stack(&mut self) {
        debug_assert!(
            !self.heap.is_borrowed(),
            "unbind the epoch Heap before resetting the helper stack"
        );
        self.frames = {
            let mut frames = ArrayVec::default();
            frames.consume();
            frames
        };
        self.frame_pins.clear();
        self.clear_dense_obj_cache();
        self.stack.seek(0);
        self.resume_stack.clear();
        self.nested_depth = 0;
        self.return_bookkeeping = false;
        self.nested_frame_depths.clear();
        self.nested_return = None;
        self.pending_ffi = None;
        self.pending_io = None;
        self.panicked = false;
        self.gc_in_progress = false;
        self.gc_deferred = false;
    }

    pub fn bind_shared_heap(
        &mut self,
        epoch: &std::sync::Arc<crate::shared_heap::SharedHeapEpoch>,
    ) {
        self.heap.bind(epoch.heap_ptr());
        self.shared_epoch = Some(std::sync::Arc::clone(epoch));
    }

    pub fn unbind_shared_heap(&mut self) {
        self.heap.unbind();
        self.shared_epoch = None;
    }

    /// Drain incremental GC and open a Layer A steal epoch on this Heap.
    pub fn begin_shared_steal(
        &mut self,
    ) -> Result<std::sync::Arc<crate::shared_heap::SharedHeapEpoch>, crate::thread::ThreadErrorTag>
    {
        if let Some(e) = &self.shared_epoch {
            e.jobs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return Ok(std::sync::Arc::clone(e));
        }
        if !self.heap.get().gc_is_idle() || self.heap.get().should_collect() {
            self.gc_collect();
        }
        if !self.heap.get().gc_is_idle() {
            return Err(crate::thread::ThreadErrorTag::Other);
        }
        let ptr = self.heap.owned_ptr();
        let epoch = crate::shared_heap::SharedHeapEpoch::new(ptr);
        self.heap.owned_mut().enter_epoch_stw(epoch.alloc_lock());
        epoch.jobs.store(1, std::sync::atomic::Ordering::SeqCst);
        self.shared_epoch = Some(std::sync::Arc::clone(&epoch));
        Ok(epoch)
    }

    /// Close one shared-heap job. `published` is the join result, which is not
    /// on the operand stack yet; Layer A collect must still treat it as a root.
    pub fn end_shared_steal(&mut self, published: Value) {
        let Some(e) = &self.shared_epoch else {
            return;
        };
        if e.jobs.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) != 1 {
            return;
        }
        // Exit STW on the epoch Heap (root owned slab, or the borrowed ptr).
        unsafe {
            (*e.heap_ptr()).exit_epoch_stw();
        }
        self.shared_epoch = None;
        if self.heap.is_borrowed() {
            return;
        }
        self.steal_join_root = published;
        if self.heap.get().should_collect() {
            self.gc_collect();
        }
        self.steal_join_root = Value::default();
    }

    /// Rewrite the first `JMP target` at or after `from` into `HALT` so setup
    /// can run without falling through into `main`.
    pub fn halt_first_jump_to(&mut self, from: usize, target: u32) {
        let owned = Arc::make_mut(&mut self.program_code);
        let code: &mut [Byte] =
            unsafe { std::slice::from_raw_parts_mut(owned.as_mut_ptr().cast(), owned.len()) };
        for b in code.iter_mut().skip(from) {
            if matches!(b.bytecode(), Instruction::JMP) && b.operand_u32() == target {
                *b = Byte::new(Instruction::HALT);
                return;
            }
        }
    }

    /// Execute loaded `program_code` starting at `start_ip` until halt/panic.
    pub fn run_from(&mut self, start_ip: usize) {
        if self.program_code.is_empty() {
            return;
        }
        let code: &[Byte] = unsafe {
            std::slice::from_raw_parts(self.program_code.as_ptr().cast(), self.program_code.len())
        };
        let constants: &[u64] = unsafe {
            std::slice::from_raw_parts(
                self.program_constants.as_ptr(),
                self.program_constants.len(),
            )
        };
        let mut ip = start_ip;
        loop {
            let paused = self.run_execute(code, constants, ip);
            if let Some(pending) = self.pending_ffi.take() {
                let resume_ip = pending.resume_ip;
                self.finish_pending_ffi_invoke(pending);
                ip = resume_ip;
                continue;
            }
            if let Some(pending) = self.pending_io.take() {
                let resume_ip = pending.resume_ip;
                self.finish_pending_io_wait(pending);
                ip = resume_ip;
                continue;
            }
            if !paused {
                break;
            }
        }
    }

    /// True when `value` is a heap `Result::Ok` (enum tag 0).
    /// Text of a boxed `Result::Err(payload)` (a string payload as is, other
    /// payloads stringified), for harness failure reports.
    pub fn result_err_text(&self, value: Value) -> Option<String> {
        match Self::find_object_by_addr(&self.heap, value.raw() as u64) {
            Some(Object::Enum(gc)) if gc.as_ref().tag == 1 => {
                let payload = gc.as_ref().payload.first().copied()?;
                Some(Self::stringify_value(&self.heap, payload))
            }
            _ => None,
        }
    }

    pub fn result_is_ok(&self, value: Value) -> bool {
        match Self::find_object_by_addr(&self.heap, value.raw() as u64) {
            Some(Object::Enum(gc)) => gc.as_ref().tag == 0,
            _ => false,
        }
    }

    pub fn run(&mut self, code: &[Byte]) {
        self.run_with_pool(code, &[], &[], 0);
    }

    /// Run bytecode with an optional constant pool for wide immediates.
    pub fn run_with_pool(
        &mut self,
        code: &[Byte],
        constants: &[u64],
        strings: &[String],
        static_slots: u32,
    ) {
        if code.is_empty() {
            return;
        }
        self.statics = vec![Value::default(); static_slots as usize];
        self.program_code = Arc::new(unsafe {
            std::slice::from_raw_parts(code.as_ptr().cast::<RawByte>(), code.len()).to_vec()
        });
        self.program_constants = Arc::new(constants.to_vec());
        self.install_program_strings(strings);
        self.sync_thread_program_from_current();
        let mut ip = 0usize;
        loop {
            let paused = self.run_execute(code, constants, ip);
            if let Some(pending) = self.pending_ffi.take() {
                let resume_ip = pending.resume_ip;
                self.finish_pending_ffi_invoke(pending);
                ip = resume_ip;
                continue;
            }
            if let Some(pending) = self.pending_io.take() {
                let resume_ip = pending.resume_ip;
                self.finish_pending_io_wait(pending);
                ip = resume_ip;
                continue;
            }
            if !paused {
                break;
            }
        }
        // Keep undetached workers alive past main's return. Without this,
        // process exit kills threads still blocked in `recv` / still starting,
        // which looks like "recv never blocks" and "nothing after recv runs".
        // Only joins *this* Machine's registry (not a process-global list).
        crate::thread::join_undetached_threads(&self.live_threads);
        // Remaining class finalizers need host IO / FFI while the reactor is
        // still up. `Drop` still runs this as a safety net for embedders.
        self.run_remaining_finalizers();
        // Reactor pool threads hold their own `Arc<Reactor>` clone and poll
        // forever unless told to stop, otherwise every program that spawns
        // a coil thread leaks `worker_cap` OS threads for the rest of the
        // process. Skip if a detached job is still in flight (rare) rather
        // than abandon queued work; that reactor just leaks as before.
        if self.reactor.inflight() == 0 {
            self.reactor.shutdown();
        }
    }

    fn finish_pending_io_wait(&mut self, pending: PendingIoWait) {
        self.frames.get_mut().set(pending.resume_sp);
        let req = pending.request;
        let wait = crate::thread::host_io_wait(req.handle, req.interest, req.timeout);
        let v = crate::host_enum::with_host_enum_layout(pending.layout, || {
            crate::io::as_result_unit(&mut self.heap, wait)
        });
        self.stack.push(v);
    }

    fn finish_pending_ffi_invoke(&mut self, pending: PendingFfiInvoke) {
        self.frames.get_mut().set(pending.resume_sp);
        let lib_obj = self.userland_libraries.get(&pending.lib_addr).copied();
        let invoke_result = match lib_obj {
            Some(obj) => {
                let l = match &obj {
                    crate::memory::Object::Library(gc) => gc,
                    _ => {
                        self.push_result_err(
                            crate::ffi::FfiErrorKindTag::InvalidHandle,
                            "invalid library handle (not a loaded library)".into(),
                        );
                        return;
                    }
                };
                let lib_ref: &crate::memory::ObjLibrary = l.as_ref();
                if pending.function_id < lib_ref.signatures.len() {
                    let registered = &lib_ref.signatures[pending.function_id];
                    let ffi_sig = registered.ffi_signature();
                    let args = match self.materialize_callback_args(&ffi_sig, &pending.args) {
                        Ok(a) => a,
                        Err(e) => {
                            self.push_ffi_error(e);
                            return;
                        }
                    };
                    let mut ctx = crate::ffi::InvokeContext::new(
                        self.heap.get_mut() as *mut Heap,
                        &self.struct_layouts,
                    );
                    let mut closure_ptrs = Vec::new();
                    crate::ffi::invoke_via_libffi(
                        &registered.prepared,
                        &ffi_sig,
                        &args,
                        pending.arg_types.as_deref(),
                        &mut ctx,
                        &mut closure_ptrs,
                    )
                } else {
                    Err(crate::ffi::FfiError::InvalidHandle(
                        "function id out of range".into(),
                    ))
                }
            }
            None => Err(crate::ffi::FfiError::InvalidHandle(
                "invalid library handle".into(),
            )),
        };
        match invoke_result {
            Ok(Some(v)) => self.push_result_ok(v),
            Ok(None) => self.push_result_ok(Value::default()),
            Err(e) => self.push_ffi_error(e),
        }
    }

    /// Push `Result::Ok(payload)` for userland FFI builtins.
    fn push_result_ok(&mut self, payload: Value) {
        let v = crate::io::alloc_result_ok(&mut self.heap, payload);
        self.stack.push(v);
    }

    /// Push `Result::Err(ffi::Error)` for userland FFI builtins.
    fn push_result_err(&mut self, kind: crate::ffi::FfiErrorKindTag, message: String) {
        let v = crate::ffi::alloc_result_ffi_err(&mut self.heap, kind, message);
        self.stack.push(v);
    }

    /// Map an [`FfiError`](crate::ffi::FfiError) into `Result::Err(ffi::Error)`.
    fn push_ffi_error(&mut self, err: crate::ffi::FfiError) {
        let kind = crate::ffi::FfiErrorKindTag::from_ffi_error(&err);
        self.push_result_err(kind, err.to_string());
    }

    /// Make room for `words` more operand words plus [`Self::frame_reserve`]
    /// above the cursor. `false` when that passes the VM limit.
    #[inline]
    fn reserve_operand_words(&mut self, words: usize) -> bool {
        self.reserve_operand_from(self.stack.tell().saturating_add(words))
    }

    /// Like [`Self::reserve_operand_words`], for a frame whose words end at `top`.
    #[inline]
    fn reserve_operand_from(&mut self, top: usize) -> bool {
        let end = top.saturating_add(self.frame_reserve);
        end <= self.stack.capacity() || self.grow_operand_stack(end)
    }

    fn grow_operand_stack_to(&mut self, end: usize) -> bool {
        end <= self.stack.capacity() || self.grow_operand_stack(end)
    }

    #[cold]
    #[inline(never)]
    fn grow_operand_stack(&mut self, end: usize) -> bool {
        if end > crate::MAX_OPERAND_STACK_SLOTS {
            return false;
        }
        let cap = self
            .stack
            .capacity()
            .saturating_mul(2)
            .clamp(end, crate::MAX_OPERAND_STACK_SLOTS);
        self.stack.grow_to(cap);
        true
    }

    /// Room for a `JumpIfMatch` payload wider than the frame reserve counts,
    /// then a fresh window above it.
    #[cold]
    #[inline(never)]
    fn reserve_wide_payload(&mut self, words: usize) -> bool {
        self.reserve_operand_words(words)
    }

    /// Recompute [`Self::frame_reserve`] when `execute` runs different code.
    fn bind_frame_reserve(&mut self, code: &[Byte], constants: &[u64]) {
        let key = (code.as_ptr().addr(), code.len());
        if self.frame_reserve_key != key {
            // SAFETY: same layout as `program_code`'s copy in `run_with_pool`.
            let raw: &[RawByte] =
                unsafe { std::slice::from_raw_parts(code.as_ptr().cast(), code.len()) };
            let reserve = common::frame_reserve(raw, constants);
            self.frame_reserve = reserve.words;
            self.match_payload_bound = reserve.match_payload;
            self.unary_bases = dispatch::UnaryBaseTable::build(code, constants);
            self.frame_reserve_key = key;
        }
    }

    /// Reserve room for the running frame, then for every frame `CALL` can
    /// open up to [`Self::call_hot_depth`] live frames. A `CALL` frame starts at
    /// most `frame_reserve` words above its caller's base (which is at or
    /// below the cursor now), so `cursor + (hot + 1) * frame_reserve` covers
    /// them all. Deeper calls, and frames opened any other way, come back
    /// here. `false` when even the running frame does not fit.
    #[inline(never)]
    fn rearm_call_window(&mut self) -> bool {
        // Without room for a window, every `CALL` takes the checked path.
        self.call_hot_depth = 0;
        let tell = self.stack.tell();
        if !self.reserve_operand_from(tell) {
            return false;
        }
        let words = self.frame_reserve.max(1);
        self.call_window_end = tell + words;
        let fit = (crate::MAX_OPERAND_STACK_SLOTS - tell) / words;
        let hot = S
            .min(self.frames.len() + CALL_WINDOW)
            .min(fit.saturating_sub(1));
        let end = tell + (hot + 1) * words;
        if hot > 0 && self.grow_operand_stack_to(end) {
            self.call_hot_depth = hot;
            self.call_window_end = end;
        }
        true
    }

    /// After frames were opened above the cursor without `CALL` (host call,
    /// closure, coroutine resume) on top of `base_depth` live frames: keep the
    /// window when it still covers them and every `CALL` it admits from them,
    /// else re-arm.
    #[inline]
    fn keep_call_window(&mut self, base_depth: usize) -> bool {
        let reach = self.call_hot_depth.saturating_sub(base_depth) + 1;
        let need = self.stack.tell() + reach * self.frame_reserve;
        need <= self.call_window_end || self.rearm_call_window()
    }

    /// `CALL` past the window: check this frame, open it, re-arm. On
    /// overflow, panics at `call_ip` and returns `false`.
    #[cold]
    #[inline(never)]
    fn open_call_frame_cold(&mut self, return_ip: usize, callee_sp: usize, call_ip: usize) -> bool {
        let opened = self.reserve_operand_words(0)
            && self.frames.rewrite_top_and_push(
                crate::MAX_CALL_FRAMES,
                |caller| caller.seek(return_ip),
                |frame| frame.set(callee_sp),
            );
        if !opened || !self.rearm_call_window() {
            return self.runtime_panic(STACK_OVERFLOW, call_ip);
        }
        true
    }

    /// Call a coil function at `offset` reentrantly (for FFI callbacks).
    pub fn call_function(&mut self, offset: u32, args: &[Value]) -> Value {
        let code: &[Byte] = unsafe {
            std::slice::from_raw_parts(self.program_code.as_ptr().cast(), self.program_code.len())
        };
        // Borrow constants without cloning; stable while `program_constants` is not resized.
        let constants: &[u64] = unsafe {
            std::slice::from_raw_parts(
                self.program_constants.as_ptr(),
                self.program_constants.len(),
            )
        };
        self.bind_frame_reserve(code, constants);
        self.fuel = self.fuel.wrapping_sub(1);
        if unlikely(self.fuel == 0) {
            self.step_budget_out(offset as usize);
            return Value::default();
        }
        if self.frames.len() >= crate::MAX_CALL_FRAMES || !self.reserve_operand_words(args.len()) {
            self.runtime_panic(STACK_OVERFLOW, offset as usize);
            return Value::default();
        }
        let saved_sp = self.stack.tell();
        for a in args {
            self.stack.push(*a);
        }
        self.nested_return = None;
        self.nested_depth += 1;
        self.return_bookkeeping = true;
        let callee_sp = self.stack.tell().saturating_sub(args.len());
        self.frames.setup_current_and_advance(|f| {
            f.seek(0);
            f.set(callee_sp);
        });
        self.keep_call_window(self.frames.len() - 1);
        // Capture only when RETURN reaches this frame depth (the
        // call_function entry), not when inner CALLs return.
        self.nested_frame_depths.push(self.frames.len());
        let mut ip = offset as usize;
        loop {
            let paused = self.run_execute(code, constants, ip);
            if let Some(pending) = self.pending_ffi.take() {
                let resume_ip = pending.resume_ip;
                self.finish_pending_ffi_invoke(pending);
                ip = resume_ip;
                continue;
            }
            if let Some(pending) = self.pending_io.take() {
                let resume_ip = pending.resume_ip;
                self.finish_pending_io_wait(pending);
                ip = resume_ip;
                continue;
            }
            if !paused {
                break;
            }
        }
        let _ = self.pop_call_frame();
        self.stack.seek(saved_sp);
        self.nested_depth -= 1;
        let _ = self.nested_frame_depths.pop();
        self.return_bookkeeping =
            self.nested_depth > 0 || !self.resume_stack.is_empty() || !self.frame_pins.is_empty();
        self.nested_return.take().unwrap_or_default()
    }

    /// Stash a return value when `execute` runs inside [`Self::call_function`].
    #[inline]
    fn capture_nested_return(&mut self, ret_val: Value) -> bool {
        // Nested FFI/host calls are rare; keep the hot RETURN path branch-free.
        if unlikely(self.return_bookkeeping) && self.nested_depth > 0 {
            let nested_target = self.nested_frame_depths.last().copied().unwrap_or(0);
            if self.frames.len() == nested_target {
                self.nested_return = Some(ret_val);
                return true;
            }
        }
        false
    }

    /// Type-erased entry for libffi callback trampolines (monomorphized per `S`).
    unsafe fn invoke_call(
        vm: *mut c_void,
        offset: u32,
        args_ptr: *const Value,
        len: usize,
    ) -> Value {
        // Edition 2024: bodies of `unsafe fn` are safe by default.
        unsafe {
            let vm = &mut *(vm.cast::<Self>());
            let args = std::slice::from_raw_parts(args_ptr, len);
            vm.call_function(offset, args)
        }
    }

    /// Run compiler-produced bytecode (archived layout, no `.hyc` round-trip).
    pub fn run_raw(
        &mut self,
        code: &[RawByte],
        constants: &[u64],
        strings: &[String],
        static_slots: u32,
    ) {
        let code: &[Byte] = unsafe { std::slice::from_raw_parts(code.as_ptr().cast(), code.len()) };
        self.run_with_pool(code, constants, strings, static_slots);
    }

    /// Never-inline: `#[inline(always)]` forced fat LTO to paste this giant
    /// `match` into `run_with_pool` / `call_function`. Whole-program context
    /// (e.g. a larger compiler in the same binary) then reshapes dispatch
    /// enough to blow branch-mispredict rates on some CPUs while keeping
    /// dynamic instruction counts identical. A single outlined copy matches
    /// the non-LTO `machine` codegen (already identical to `main`'s).
    /// Fused jump tables live *inside* this outlined copy. Bytecode prefetch
    /// was removed: the next word is already in L1 on the flagship loops, and
    /// the guard compare retired on every dispatch.
    /// An always-hot arm continues the streak in `execute_dense`, so a dense
    /// loop does not return here per opcode. CALL/RETURN stay on this match.
    /// [`Self::execute`], counted as active for the evacuation re-entrancy
    /// check (outside the loop, so it costs nothing per op).
    #[inline(always)]
    fn run_execute(&mut self, code: &[Byte], constants: &[u64], start_ip: usize) -> bool {
        IN_EXECUTE.set(IN_EXECUTE.get() + 1);
        let paused = self.execute(code, constants, start_ip);
        IN_EXECUTE.set(IN_EXECUTE.get() - 1);
        paused
    }

    #[inline(never)]
    fn execute(&mut self, code: &[Byte], constants: &[u64], start_ip: usize) -> bool {
        let _active_guard = crate::thread::HostStateGuard::enter(self);

        let mut ip: usize = start_ip;
        let mut sp = self.frames.get_mut().get();
        self.bind_frame_reserve(code, constants);
        if !self.rearm_call_window() {
            return self.runtime_panic(STACK_OVERFLOW, ip);
        }
        // Bound for `promise!` index hints only. The buffer grows during the
        // run but never past this, so the hints stay true without a refresh.
        let stack_cap = crate::MAX_OPERAND_STACK_SLOTS;
        let code_len = code.len();

        // A debugger checks breakpoints / steps before every instruction:
        // never run a dense streak past it.
        #[cfg(any(test, feature = "debugger"))]
        let streaks_allowed = self.debug.is_none();
        #[cfg(not(any(test, feature = "debugger")))]
        let streaks_allowed = true;
        // Coverage counts at the main dispatch only: no streaks past it either.
        #[cfg(any(test, feature = "coverage"))]
        let streaks_allowed = streaks_allowed && self.coverage.is_none();

        macro_rules! then_hot_streak {
            () => {
                // Inline peek: non-dense code (tak, fib) must not pay the
                // outlined streak call after every jump.
                if streaks_allowed
                    && ip < code_len
                    && dispatch::is_always_hot_disc(
                        // SAFETY: `ip < code_len` checked above.
                        *unsafe { code.get_unchecked(ip) }.bytecode() as u8,
                    )
                    && let Some(msg) = dispatch::consume_always_hot_streak(
                    dispatch::ConsumeAlwaysHotStreakArgs {
                        stack: &mut self.stack,
                        sp: &mut sp,
                        ip: &mut ip,
                        code,
                        constants,
                        heap: &mut self.heap,
                        frames: &mut self.frames,
                        frame_pins: &mut self.frame_pins,
                        dense_obj_addr: &mut self.dense_obj_addr,
                        dense_obj: &mut self.dense_obj,
                        fuel: &mut self.fuel,
                        stack_cap,
                    },
                ) {
                    return self.hot_streak_panic(msg, ip.saturating_sub(1));
                }
                if unlikely(!self.frame_pins.is_empty()) {
                    self.return_bookkeeping = true;
                }
            };
        }

        // Charge one step: `$back` is `true` for a call, or `target < ip`
        // (`ip` is past the jump word) for a jump. Branch-free decrement;
        // the zero check is the only branch.
        macro_rules! charge_step {
            ($back:expr) => {
                self.fuel = self.fuel.wrapping_sub(($back) as u64);
                if unlikely(self.fuel == 0) {
                    return self.step_budget_out(ip.saturating_sub(1));
                }
            };
        }
        // Jump to `$target`, charging a back-edge.
        macro_rules! jump {
            ($target:expr) => {{
                let target: usize = $target;
                charge_step!(target < ip);
                set_jump_target(&mut ip, target, code);
            }};
        }

        while ip < code_len {
            #[cfg(any(test, feature = "debugger"))]
            if unlikely(self.debug.is_some())
                && let Some(reason) = self.debug_check_stop_at(ip)
            {
                self.frames.get_mut().seek(ip);
                self.frames.get_mut().set(sp);
                self.pending_debug_stop = Some(reason);
                return true;
            }

            #[cfg(any(test, feature = "debugger"))]
            let debug_attached = self.debug.is_some();
            #[cfg(not(any(test, feature = "debugger")))]
            let debug_attached = false;

            note_dispatch_at(ip, &self.stack, sp);

            #[cfg(any(test, feature = "coverage"))]
            if unlikely(self.coverage.is_some()) {
                self.note_coverage(ip);
            }

            // SAFETY: loop condition guarantees `ip < code.len()`.
            promise!(ip < code_len);
            let opcode = unsafe { code.get_unchecked(ip) };
            ip += 1;
            prefetch_code(code, ip);

            let bc = opcode.bytecode();
            // Release-only optimizer hint: must track the LAST `Instruction`
            // variant. A stale ceiling (e.g. YieldFromCoro) makes later opcodes
            // (`StoreIndex`, `DoneCoro`, `ArrayPush`, …) UB via assert_unchecked.
            #[cfg(not(debug_assertions))]
            promise!(*bc as u8 <= Instruction::MakeArrayK as u8);

            match bc {
                Instruction::STORE => {
                    // Pop TOS into each listed slot (packed n=1..=3, or wide n=0).
                    // After all pops, keep the shared operand/local cursor at or
                    // past the highest written slot so later pushes do not
                    // clobber multi-slot locals (fixed `[T; N]` on stack).
                    dispatch::store(&mut self.stack, sp, opcode, stack_cap);
                }
                Instruction::Seek => {
                    // Frame-relative cursor: operands[31:0] = slot offset from `sp`.
                    dispatch::seek(&mut self.stack, sp, opcode, stack_cap);
                }
                Instruction::LOAD => {
                    dispatch::load(&mut self.stack, sp, opcode, stack_cap);
                }
                Instruction::JMP => {
                    jump!(opcode.operand_u32() as usize);
                    then_hot_streak!();
                }
                Instruction::JMPF => {
                    if !self.stack.pop().as_bool() {
                        jump!(opcode.operand_u32() as usize);
                    }
                    then_hot_streak!();
                }
                Instruction::JMPT => {
                    if self.stack.pop().as_bool() {
                        jump!(opcode.operand_u32() as usize);
                    }
                    then_hot_streak!();
                }
                Instruction::CALL => {
                    let (arity, target) = opcode.call_parts();
                    promise!(self.stack.tell() >= arity);
                    // Empty set is the hot path (fib); skip HashSet::contains.
                    if arity == 1
                        && unlikely(!self.finalizer_pcs.is_empty())
                        && self.finalizer_pcs.contains(&(target as u32))
                    {
                        promise!(self.stack.tell() >= 1);
                        let self_val = self.stack[self.stack.tell() - 1];
                        if !self.claim_finalizer(self_val) {
                            self.stack.pop();
                            self.stack.push(Value::from(0i64));
                            continue;
                        }
                    }
                    let callee_sp = self.stack.tell() - arity;
                    // Direct calls dominate; avoid the indirect `target == 0`
                    // return-ip adjustment on that path.
                    // Unary callee that is `if arg ? imm { return k }` does not
                    // need a frame. Debugger stays on the real call so a stop
                    // on the base `ConstReturnImm` still fires.
                    if !debug_attached
                        && arity == 1
                        && target != 0
                        && let Some(ret) = self.unary_bases.fast_return(
                            target,
                            self.stack[self.stack.tell() - 1],
                            &self.heap,
                        )
                    {
                        self.stack.pop();
                        self.stack.push(ret);
                        continue;
                    }
                    // Inside the window the stack already has room: one compare,
                    // the same as the inline-frame check it replaces.
                    macro_rules! open_frame {
                        ($return_ip:expr) => {
                            if likely(self.frames.len() < self.call_hot_depth) {
                                self.frames.rewrite_top_and_push_inline(
                                    |caller| caller.seek($return_ip),
                                    |frame| frame.set(callee_sp),
                                );
                            } else if !self.open_call_frame_cold($return_ip, callee_sp, ip - 1) {
                                return false;
                            }
                        };
                    }
                    charge_step!(true);
                    if likely(target != 0) {
                        open_frame!(ip);
                        sp = callee_sp;
                        set_jump_target(&mut ip, target, code);
                    } else {
                        open_frame!(ip + 1);
                        sp = callee_sp;
                    }
                }
                Instruction::TailCall => {
                    let (arity, target) = opcode.call_parts();
                    charge_step!(true);
                    promise!(self.stack.tell() >= arity);
                    let callee_sp = self.frames.get().get();
                    let src = self.stack.tell() - arity;
                    // Args sit at TOS; frame base is at or below them.
                    self.stack.copy_slots(callee_sp, src, arity);
                    self.stack.seek(callee_sp + arity);
                    // Match CALL: `sp` is the frame base (locals start at slot 0),
                    // not past the args. Using `callee_sp + arity` would make
                    // subsequent LOAD/BinSlotImm read the wrong slots.
                    sp = callee_sp;
                    set_jump_target(&mut ip, target, code);
                }
                Instruction::RETURN => {
                    if unlikely(opcode.return_words() >= 2) {
                        // Two-slot `[payload, tag]` return (tag on top).
                        // Direct CALL/RETURN of a known ≤2-word layout never
                        // crosses a `call_function` re-entrant boundary (FFI/
                        // coroutine targets always go through a boxed
                        // wrapper), so `capture_nested_return` never needs
                        // the tag here.
                        promise!(self.stack.tell() >= 2);
                        let tag = self.stack.pop();
                        let payload = self.stack.pop();
                        if self.capture_nested_return(payload) {
                            return false;
                        }
                        let return_sp = self.pop_call_frame();
                        self.stack.seek(return_sp);
                        self.stack.push(payload);
                        self.stack.push(tag);
                        self.after_return(&mut ip, &mut sp);
                    } else {
                        let ret_val = self.stack.pop();
                        if self.capture_nested_return(ret_val) {
                            return false;
                        }
                        let return_sp = self.pop_call_frame();
                        self.stack.seek(return_sp);
                        self.stack.push(ret_val);
                        self.after_return(&mut ip, &mut sp);
                    }
                }
                // (same shape as `BinSlotSlot`) to avoid two temp pushes.
                Instruction::BinSlotImm => {
                    dispatch::bin_slot_imm(&mut self.stack, sp, opcode, &self.heap, stack_cap);
                }
                Instruction::CmpJmpf | Instruction::CmpJmpt => {
                    if let Some(target) = dispatch::cmp_jmp(
                        &mut self.stack,
                        opcode,
                        constants,
                        &self.heap,
                        matches!(*bc, Instruction::CmpJmpt),
                    ) {
                        jump!(target);
                    }
                    then_hot_streak!();
                }
                // Fused `LOAD slot; CONST imm; <cond>; JMPF/JMPT` without stack traffic.
                Instruction::BinSlotImmJmpf | Instruction::BinSlotImmJmpt => {
                    if let Some(target) = dispatch::bin_slot_imm_jmp(
                        &self.stack,
                        sp,
                        opcode,
                        constants,
                        &self.heap,
                        stack_cap,
                        matches!(*bc, Instruction::BinSlotImmJmpt),
                    ) {
                        jump!(target);
                    }
                }
                Instruction::LogNotJmpf | Instruction::LogNotJmpt => {
                    if let Some(target) = dispatch::log_not_jmp(
                        &mut self.stack,
                        opcode,
                        constants,
                        matches!(*bc, Instruction::LogNotJmpt),
                    ) {
                        jump!(target);
                    }
                    then_hot_streak!();
                }
                // Fused `BinSlotSlot; JMPF/JMPT`, pool packs (target<<32)|b.
                Instruction::BinSlotSlotJmpf | Instruction::BinSlotSlotJmpt => {
                    if let Some(target) = dispatch::bin_slot_slot_jmp(
                        &self.stack,
                        sp,
                        opcode,
                        constants,
                        &self.heap,
                        stack_cap,
                        matches!(*bc, Instruction::BinSlotSlotJmpt),
                    ) {
                        jump!(target);
                    }
                    then_hot_streak!();
                }
                // Fused `LOAD src; CONST imm; <op>; STORE dest`, pool packs (dest<<32)|imm.
                Instruction::BinSlotImmStore => {
                    dispatch::bin_slot_imm_store(
                        &mut self.stack,
                        sp,
                        opcode,
                        constants,
                        &self.heap,
                        stack_cap,
                    );
                }
                Instruction::BinSlotSlotStore => {
                    dispatch::bin_slot_slot_store(
                        &mut self.stack,
                        sp,
                        opcode,
                        &self.heap,
                        stack_cap,
                    );
                    then_hot_streak!();
                }
                Instruction::LoadReturnSlot => {
                    let slot = opcode.operand_u32() as usize;
                    promise!(sp + slot < stack_cap);
                    let ret_val = self.stack[sp + slot];
                    if self.capture_nested_return(ret_val) {
                        return false;
                    }
                    let return_sp = self.pop_call_frame();
                    self.stack.seek(return_sp);
                    self.stack.push(ret_val);
                    self.after_return(&mut ip, &mut sp);
                }
                Instruction::ConstReturnImm => {
                    let ret_val = Value::from(opcode.operand_u32() as i32 as i64 as u64);
                    if self.capture_nested_return(ret_val) {
                        return false;
                    }
                    let return_sp = self.pop_call_frame();
                    self.stack.seek(return_sp);
                    self.stack.push(ret_val);
                    self.after_return(&mut ip, &mut sp);
                }
                Instruction::BinReturn => {
                    // Compute result without leaving an intermediate TOS;
                    // return unwind reseeks the stack anyway.
                    let tos = self.stack.tell();
                    promise!(tos >= 2);
                    let rhs = self.stack[tos - 1];
                    let lhs = self.stack[tos - 2];
                    let ret_val =
                        crate::fused::eval_bin(opcode.bin_return_op(), lhs, rhs, &self.heap);
                    if self.capture_nested_return(ret_val) {
                        return false;
                    }
                    let return_sp = self.pop_call_frame();
                    self.stack.seek(return_sp);
                    self.stack.push(ret_val);
                    self.after_return(&mut ip, &mut sp);
                }
                Instruction::MakeEnumReturn | Instruction::MakeEnumReturnK => {
                    self.push_make_enum(opcode, ip);
                    let ret_val = self.stack.pop();
                    if self.capture_nested_return(ret_val) {
                        return false;
                    }
                    let return_sp = self.pop_call_frame();
                    self.stack.seek(return_sp);
                    self.stack.push(ret_val);
                    self.after_return(&mut ip, &mut sp);
                }
                Instruction::DenseBin | Instruction::DenseBin2 => {
                    dispatch::dense_bin(&mut self.stack, sp, opcode, stack_cap);
                    if unlikely(*bc as u8 == Instruction::DenseBin2 as u8) {
                        promise!(ip < code_len);
                        let tail = unsafe { code.get_unchecked(ip) };
                        ip += 1;
                        prefetch_code(code, ip);
                        dispatch::dense_bin(&mut self.stack, sp, tail, stack_cap);
                    }
                    then_hot_streak!();
                }
                Instruction::DenseBinJmpf => {
                    dispatch::dense_bin(&mut self.stack, sp, opcode, stack_cap);
                    promise!(ip < code_len);
                    let tail = unsafe { code.get_unchecked(ip) };
                    ip += 1;
                    prefetch_code(code, ip);
                    if let Some(target) = dispatch::dense_bin_jmp_tail(
                        &mut self.stack,
                        sp,
                        tail,
                        constants,
                        &self.heap,
                        stack_cap,
                    ) {
                        jump!(target);
                    }
                    then_hot_streak!();
                }
                Instruction::DenseCmp => {
                    dispatch::dense_cmp(&mut self.stack, sp, opcode, stack_cap);
                    then_hot_streak!();
                }
                Instruction::DenseConst => {
                    dispatch::dense_const(&mut self.stack, sp, opcode, constants, stack_cap);
                    then_hot_streak!();
                }
                Instruction::DenseMove => {
                    dispatch::dense_move(&mut self.stack, sp, opcode, stack_cap);
                    then_hot_streak!();
                }
                Instruction::DenseUnary => {
                    dispatch::dense_unary(&mut self.stack, sp, opcode, stack_cap);
                    then_hot_streak!();
                }
                Instruction::DenseCast => {
                    dispatch::dense_cast(&mut self.stack, sp, opcode, stack_cap);
                    then_hot_streak!();
                }
                Instruction::DenseIndex | Instruction::DenseIndexJmpf => {
                    if dispatch::dense_index(dispatch::DenseIndexArgs {
                        stack: &mut self.stack,
                        sp,
                        opcode,
                        heap: &self.heap,
                        frames_len: self.frames.len(),
                        frame_pins: &mut self.frame_pins,
                        dense_obj_addr: &mut self.dense_obj_addr,
                        dense_obj: &mut self.dense_obj,
                        stack_cap,
                    })
                    .is_err()
                    {
                        return self.runtime_panic("index out of bounds", ip.saturating_sub(1));
                    }
                    if unlikely(*bc as u8 == Instruction::DenseIndexJmpf as u8) {
                        promise!(ip < code_len);
                        let tail = unsafe { code.get_unchecked(ip) };
                        ip += 1;
                        prefetch_code(code, ip);
                        if let Some(target) = dispatch::dense_bin_jmp_tail(
                            &mut self.stack,
                            sp,
                            tail,
                            constants,
                            &self.heap,
                            stack_cap,
                        ) {
                            jump!(target);
                        }
                    }
                    then_hot_streak!();
                }
                Instruction::DenseStoreIndex => {
                    match dispatch::dense_store_index(dispatch::DenseStoreIndexArgs {
                        stack: &mut self.stack,
                        sp,
                        opcode,
                        heap: &self.heap,
                        frames_len: self.frames.len(),
                        frame_pins: &mut self.frame_pins,
                        dense_obj_addr: &mut self.dense_obj_addr,
                        dense_obj: &mut self.dense_obj,
                        stack_cap,
                    }) {
                        Ok(()) => {}
                        Err(dispatch::DenseFail::IndexOob) => {
                            return self.runtime_panic("index out of bounds", ip.saturating_sub(1));
                        }
                        Err(_) => {
                            return self
                                .runtime_panic("StoreIndex on non-array", ip.saturating_sub(1));
                        }
                    }
                    then_hot_streak!();
                }
                Instruction::DenseArrayLen => {
                    dispatch::dense_array_len(&mut self.stack, sp, opcode, &self.heap, stack_cap);
                    then_hot_streak!();
                }
                Instruction::DenseFieldLoad => {
                    if dispatch::dense_field_load(
                        &mut self.stack,
                        sp,
                        opcode,
                        &mut self.heap,
                        stack_cap,
                    )
                    .is_err()
                    {
                        return self.runtime_panic("no such field", ip.saturating_sub(1));
                    }
                    then_hot_streak!();
                }
                Instruction::DenseFieldStore => {
                    match dispatch::dense_field_store(
                        &mut self.stack,
                        sp,
                        opcode,
                        &mut self.heap,
                        stack_cap,
                    ) {
                        Ok(()) => {}
                        Err(_) => {
                            return self
                                .runtime_panic("SetField on non-instance", ip.saturating_sub(1));
                        }
                    }
                    then_hot_streak!();
                }
                Instruction::StorePop => {
                    let count = opcode.load_store_count();
                    for i in 0..count {
                        let slot = sp + opcode.load_store_slot_at(i) as usize;
                        promise!(slot < stack_cap);
                        let val = self.stack.pop();
                        self.stack[slot] = val;
                        let tell = self.stack.tell();
                        if tell < slot + 1 {
                            self.stack.seek(slot + 1);
                        }
                    }
                }
                _ => match self.exec_rest(opcode, &mut ip, &mut sp, code, constants, stack_cap) {
                    dispatch::RestFlow::Continue => {}
                    dispatch::RestFlow::Done(paused) => return paused,
                },
            }
        }
        false
    }
}

include!("exec_rest.rs");

impl<const S: usize> Drop for Machine<S> {
    fn drop(&mut self) {
        self.run_remaining_finalizers();
    }
}

#[cfg(test)]
#[path = "vm.tests.rs"]
mod tests;
