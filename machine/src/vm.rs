//! Bytecode interpreter: dispatch loop, automatic GC, and FFI.

use std::{
    ffi::c_void,
    fmt::Write as FmtWrite,
    io::{self, Write as IoWrite},
    path::PathBuf,
};

#[cfg(any(test, feature = "vm_profile"))]
use std::sync::atomic::{AtomicU64, Ordering};

use common::{
    ArchivedByte as Byte, ArchivedInstruction as Instruction, ArrayVec, Byte as RawByte,
    ProgramDebug, Value, byte_to_position, likely, promise, set_field_slot_index, unlikely,
    unpack_init_typed,
};

use crate::{
    AddrHashBuilder, CStructLayout, CoroState, Frame, GcData, Heap, Member, ObjArray, ObjBoxed,
    EnumPayload, ObjCoroutine, ObjEnum, ObjFn, ObjInstance, ObjPolyFn, ObjString, ObjTuple, Object,
    RefCoroutine,
    Stack,
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

/// Software-prefetch the bytecode word at `ip` (no-op if past the end).
/// `core::hint::prefetch_read` is still unstable (`hint_prefetch`).
/// `_mm_prefetch` is stable on x86_64; `core::arch::aarch64::_prefetch` is
/// not (`stdarch_aarch64_prefetch`). Use `prfm` via stable `asm!` instead.
#[inline(always)]
fn prefetch_code(code: &[Byte], ip: usize) {
    if ip >= code.len() {
        return;
    }
    let ptr = unsafe { code.as_ptr().add(ip) };
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::x86_64::_mm_prefetch::<{ core::arch::x86_64::_MM_HINT_T1 }>(ptr.cast::<i8>());
    }
    // pldl2keep ≈ x86 T1: L2, do not shove the operand stack out of L1.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!(
            "prfm pldl2keep, [{ptr}]",
            ptr = in(reg) ptr,
            options(readonly, nostack, preserves_flags),
        );
    }
}

#[inline(always)]
fn set_jump_target(ip: &mut usize, target: usize, code: &[Byte]) {
    // Lowering may target `code.len()` as “fall out of the loop” (next `while`
    // check exits without fetching).
    promise!(target <= code.len());
    *ip = target;
    prefetch_code(code, target);
}

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
}

/// One frame's pinned arrays, keyed by `ArrayPin` operand (local slot).
///
/// Allocated lazily on first `ArrayPin`. Lookup is a vec index, not a hash.
struct FramePins {
    /// `frames.len()` at first pin (TailCall keeps the same depth).
    depth: usize,
    by_slot: Vec<Option<Object>>,
}

pub struct Machine<const S: usize> {
    heap: Heap,
    stack: Stack<Value>,
    frames: ArrayVec<Frame, S>,
    /// Pin tables for frames that actually ran `ArrayPin`.
    frame_pins: Vec<FramePins>,
    output: Option<OutputSink>,
    natives: crate::ffi::Natives,
    libraries: std::collections::HashMap<String, std::sync::Arc<crate::ffi::Library>>,
    userland_libraries: std::collections::HashMap<u64, std::sync::Arc<Object>, AddrHashBuilder>,
    resume_stack: Vec<ResumeCtx>,
    /// Directory of the entry script (for relative `dload` paths).
    base_dir: Option<PathBuf>,
    /// Extra search paths from `coil.toml` `[ffi]`.
    ffi_search_paths: Vec<PathBuf>,
    /// Fail-closed `dload` integrity (lock hash or trusted).
    dload_gate: crate::ffi::DloadGate,
    pgo: crate::pgo::PgoCounters,
    /// Registered C struct layouts for pass-by-value FFI.
    struct_layouts: Vec<CStructLayout>,
    /// Keeps libffi callback trampolines alive (ties lifetime to VM run).
    ffi_closures: Vec<crate::ffi::OwnedClosure>,
    /// Bytecode/constants for nested `call_function` / callbacks.
    program_code: Vec<RawByte>,
    program_constants: Vec<u64>,
    program_strings: Vec<String>,
    /// Interned handle per `program_strings` index. Not a GC root: sweep
    /// zeros the table so unmarked literals can die; STRING still stacks
    /// the handle before `maybe_gc`.
    program_string_cache: Vec<Value>,
    /// When > 0, `RETURN` captures into `nested_return` instead of unwinding to caller.
    nested_depth: u32,
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
    /// `type_id` → drop method entry PC (empty = no user finalizers).
    finalizer_by_type: std::collections::HashMap<u32, u32, AddrHashBuilder>,
    /// Drop entry PCs (for explicit `obj.drop()` once-bit intercept).
    finalizer_pcs: std::collections::HashSet<u32, AddrHashBuilder>,
    /// True while a mark/finalize/sweep cycle is running.
    gc_in_progress: bool,
    /// Nested `gc_collect` during a finalizer; run another cycle after.
    gc_deferred: bool,
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
            heap: Heap::default(),
            stack: Stack::with_capacity(cap),
            output: None,
            natives: crate::ffi::Natives::new(),
            libraries: std::collections::HashMap::new(),
            userland_libraries: std::collections::HashMap::default(),
            resume_stack: Vec::new(),
            base_dir: None,
            ffi_search_paths: Vec::new(),
            dload_gate: crate::ffi::DloadGate::deny_all(),
            pgo: crate::pgo::PgoCounters::new(),
            struct_layouts: Vec::new(),
            ffi_closures: Vec::new(),
            program_code: Vec::new(),
            program_constants: Vec::new(),
            program_strings: Vec::new(),
            program_string_cache: Vec::new(),
            nested_depth: 0,
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
            thread_program: None,
            shared_print: None,
            live_threads: crate::thread::new_live_thread_registry(),
            worker_cap,
            reactor,
            io_reactor: crate::io_reactor::IoReactor::new(),
            finalizer_by_type: std::collections::HashMap::default(),
            finalizer_pcs: std::collections::HashSet::default(),
            gc_in_progress: false,
            gc_deferred: false,
        }
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

    pub fn pgo_counters(&self) -> &crate::pgo::PgoCounters {
        &self.pgo
    }

    pub fn pgo_snapshot(&self) -> crate::pgo::PgoSnapshot {
        self.pgo.snapshot()
    }

    pub fn pgo_reset(&self) {
        self.pgo.reset();
    }

    pub fn set_pgo_counters(&mut self, pgo: crate::pgo::PgoCounters) {
        self.pgo = pgo;
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
        self.panicked = false;
        self.pending_ffi = None;
        self.pending_io = None;
        self.pending_debug_stop = None;
        self.nested_depth = 0;
        self.nested_frame_depths.clear();
        self.nested_return = None;
        self.resume_stack.clear();
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
            self.program_code = unsafe {
                std::slice::from_raw_parts(code.as_ptr().cast::<RawByte>(), code.len()).to_vec()
            };
            self.program_constants = constants.to_vec();
            self.install_program_strings(strings);
            self.sync_thread_program_from_current();
        }
        let mut ip = start_ip;
        loop {
            self.pending_debug_stop = None;
            let paused = self.execute(code, constants, ip);
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

    fn format_panic_backtrace(&self, panic_insn_ip: usize) -> String {
        let mut lines = Vec::new();
        if let Some(loc) = self.format_panic_location(panic_insn_ip) {
            lines.push(format!("  at {loc}"));
        }
        for frame_idx in (0..self.frames.len()).rev() {
            let ip = self.frames[frame_idx].tell();
            let name = self.fn_symbol_at_ip(ip).unwrap_or("<unknown>");
            if let Some(loc) = self.format_panic_location(ip) {
                lines.push(format!("  in {name} at {loc}"));
            } else {
                lines.push(format!("  in {name}"));
            }
        }
        lines.join("\n")
    }

    /// Abort execution with a VM panic (same path as `Instruction::Panic`).
    fn runtime_panic(&mut self, message: &str, panic_insn_ip: usize) -> bool {
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
                eprintln!("{backtrace}");
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

    pub fn register_struct_layout(&mut self, layout: CStructLayout) -> u32 {
        let id = self.struct_layouts.len() as u32;
        self.struct_layouts.push(layout);
        id
    }

    // pub fn register(&mut self, name: usize, func: External) {
    //     self.native.insert(name, func);
    // }

    /// Free function so `execute` can borrow `frames` and `heap` separately.
    /// Delegates to [`Heap::find_object_by_addr`] (mapped slot + header kind).
    fn find_object_by_addr(heap: &Heap, addr: u64) -> Option<Object> {
        heap.find_object_by_addr(addr)
    }

    /// `ObjEnum` at an exact slot. Heap-heap Result `Err` (`pointer | 1`) is
    /// not an enum cell — do not strip bit 0 here (GC marking already does).
    fn find_enum_exact(heap: &Heap, addr: u64) -> Option<crate::memory::Gc<crate::memory::ObjEnum>> {
        if addr & 1 != 0 {
            return None;
        }
        match Self::find_object_by_addr(heap, addr) {
            Some(Object::Enum(e)) => Some(e),
            _ => None,
        }
    }

    fn pop_call_frame(&mut self) -> usize {
        self.pop_pin_map_for_current_frame();
        self.frames.pop().get()
    }

    /// Drop the pin table for the active frame if ArrayPin created one.
    #[inline]
    fn pop_pin_map_for_current_frame(&mut self) {
        let depth = self.frames.len();
        if self.frame_pins.last().is_some_and(|p| p.depth == depth) {
            self.frame_pins.pop();
        }
    }

    #[inline]
    fn pinned_object(&self, slot: u32) -> Option<Object> {
        let depth = self.frames.len();
        let pins = self.frame_pins.last()?;
        if pins.depth != depth {
            return None;
        }
        pins.by_slot.get(slot as usize).copied().flatten()
    }

    /// Allocate a pin table only when this frame first pins an array.
    #[inline]
    fn pin_current_array(&mut self, slot: u32, obj: Object) {
        let depth = self.frames.len();
        let idx = slot as usize;
        if let Some(pins) = self.frame_pins.last_mut() {
            if pins.depth == depth {
                if pins.by_slot.len() <= idx {
                    pins.by_slot.resize(idx + 1, None);
                }
                pins.by_slot[idx] = Some(obj);
                return;
            }
        }
        let mut by_slot = vec![None; idx + 1];
        by_slot[idx] = Some(obj);
        self.frame_pins.push(FramePins { depth, by_slot });
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

    fn write_indexed(elements: &mut [Value], index: i64, value: Value, unchecked: bool) -> bool {
        let len = elements.len();
        if unchecked {
            let idx = index as usize;
            promise!(index >= 0);
            promise!(idx < len);
            unsafe {
                *elements.get_unchecked_mut(idx) = value;
            }
            true
        } else if index >= 0 && (index as usize) < len {
            unsafe {
                *elements.get_unchecked_mut(index as usize) = value;
            }
            true
        } else {
            false
        }
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
        use crate::ffi::{VmCallFn, callback_cif, make_int_callback};
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
        self.userland_libraries
            .insert(addr, std::sync::Arc::new(object));
        self.libraries
            .entry(path.to_string())
            .or_insert_with(|| lib_arc.clone());
        Ok(Value::from(addr as *mut u8))
    }

    /// Mark-and-sweep GC, running registered class finalizers after mark.
    fn gc_collect(&mut self) {
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

            self.heap.clear_dead_weaks();
            // SAFETY: all reachable objects were marked above; dead weaks cleared.
            unsafe { self.heap.sweep() };
            // Cache is not a GC root; unmarked interned literals are gone.
            self.program_string_cache.fill(Value::default());
            if !self.gc_deferred {
                break;
            }
        }
        self.gc_in_progress = false;
    }

    fn collect_vm_root_addrs(&mut self) -> Vec<u64> {
        let mut roots = self.heap.take_gc_roots();
        for v in self.stack.as_slice() {
            let addr = v.heap_addr();
            if addr != 0 && self.heap.find_object_by_addr(addr).is_some() {
                roots.push(addr);
            }
        }
        for v in &self.statics {
            let addr = v.heap_addr();
            if addr != 0 && self.heap.find_object_by_addr(addr).is_some() {
                roots.push(addr);
            }
        }
        for ctx in &self.resume_stack {
            roots.push(ctx.coro.as_ptr() as u64);
        }
        for obj in self.heap.into_iter() {
            if let Object::Coroutine(gc) = obj {
                roots.push(gc.as_ptr() as u64);
                Self::root_coroutine_saved_stack(&self.heap, gc.as_ref(), &mut roots);
            }
        }
        for pins in &self.frame_pins {
            for obj in pins.by_slot.iter().flatten() {
                roots.push(obj.addr());
            }
        }
        // `FfiLoad` keeps `ObjLibrary` in `userland_libraries` for the VM
        // lifetime; the Coil handle is only an addr. Root those keys so GC
        // cannot sweep a live dload and `FfiInvoke` hit `invalid library handle`.
        roots.extend(self.userland_libraries.keys().copied());
        roots
    }

    fn mark_from_vm_roots(&mut self) {
        let roots = self.collect_vm_root_addrs();
        self.heap.mark_from_roots(&roots);
        self.heap.restore_gc_roots(roots);
    }

    fn unmark_heap(&self) {
        let mut current = self.heap.head_for_lookup();
        while let Some(obj) = current {
            obj.unmark();
            current = obj.get_next();
        }
    }

    fn queue_unmarked_finalizers(&self) -> Vec<(Value, u32)> {
        if self.finalizer_by_type.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut current = self.heap.head_for_lookup();
        while let Some(obj) = current {
            if !obj.is_marked()
                && let Object::Instance(gc) = obj
            {
                let inst = gc.as_ref();
                if inst.type_id != 0
                    && !inst.finalized
                    && let Some(&pc) = self.finalizer_by_type.get(&inst.type_id)
                {
                    out.push((Value::from(obj.addr()), pc));
                }
            }
            current = obj.get_next();
        }
        out
    }

    fn claim_finalizer(&self, v: Value) -> bool {
        match Self::find_object_by_addr(&self.heap, v.raw() as u64) {
            Some(Object::Instance(gc)) => {
                let inst = gc.payload_mut();
                if inst.finalized {
                    false
                } else {
                    inst.finalized = true;
                    true
                }
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
        let mut current = self.heap.head_for_lookup();
        while let Some(obj) = current {
            if let Object::Instance(gc) = obj {
                let inst = gc.as_ref();
                if inst.type_id != 0
                    && !inst.finalized
                    && let Some(&pc) = self.finalizer_by_type.get(&inst.type_id)
                {
                    queue.push((Value::from(obj.addr()), pc));
                }
            }
            current = obj.get_next();
        }
        for (val, pc) in queue {
            self.run_finalizer(val, pc);
        }
    }

    /// Run GC when live heap bytes exceed the heap threshold.
    #[inline]
    fn maybe_gc_after_alloc(&mut self) {
        if unlikely(self.heap.should_collect()) {
            self.gc_collect();
        }
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
    fn stack_copy_enum_payload(
        heap: &Heap,
        stack: &Stack<Value>,
        sp: usize,
        n: usize,
    ) -> EnumPayload {
        match n {
            0 => EnumPayload::empty(),
            1 => EnumPayload::one(Self::value_as_member(heap, stack[sp - 1])),
            2 => EnumPayload::two(
                Self::value_as_member(heap, stack[sp - 1]),
                Self::value_as_member(heap, stack[sp - 2]),
            ),
            _ => {
                let mut payload = Vec::with_capacity(n);
                for i in 0..n {
                    payload.push(Self::value_as_member(heap, stack[sp - 1 - i]));
                }
                EnumPayload::from_vec(payload)
            }
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

    fn root_coroutine_saved_stack(heap: &Heap, coro: &ObjCoroutine, roots: &mut Vec<u64>) {
        let mask = coro.saved_live_mask;
        for (i, v) in coro.saved_stack.iter().enumerate() {
            if mask != 0 && i < 64 && mask & (1u64 << i) == 0 {
                continue;
            }
            let addr = v.heap_addr();
            if addr != 0 && heap.find_object_by_addr(addr).is_some() {
                roots.push(addr);
            }
        }
        if let Some(delegate) = &coro.yield_from {
            roots.push(delegate.as_ptr() as u64);
        }
    }

    /// Intern `data`, push the GC pointer, then maybe collect.
    ///
    /// The intern table is a cache, not a GC root — unmarked interned strings
    /// are swept. The new object must be on the operand stack before
    /// [`Self::gc_collect`] so it survives the cycle.
    fn push_interned_string(&mut self, data: String) {
        let gc_string = self.heap.intern(data);
        self.stack
            .push(Value::from(gc_string.as_ptr() as *mut u8 as u64));
        self.maybe_gc_after_alloc();
    }

    fn install_program_strings(&mut self, strings: &[String]) {
        self.program_strings = strings.to_vec();
        self.program_string_cache.clear();
        self.program_string_cache
            .resize(self.program_strings.len(), Value::default());
    }

    fn push_program_string(&mut self, idx: usize) {
        let cached = unsafe { *self.program_string_cache.get_unchecked(idx) };
        if likely(!cached.raw().is_null()) {
            self.stack.push(cached);
            return;
        }
        let data = unsafe { self.program_strings.get_unchecked(idx) };
        let gc_string = self.heap.intern_str(data);
        let handle = Value::from(gc_string.as_ptr() as *mut u8 as u64);
        self.stack.push(handle);
        self.maybe_gc_after_alloc();
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
        self.thread_program = Some(program);
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
        &mut self.heap
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
            pgo: self.pgo.clone(),
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
            code: std::sync::Arc::new(self.program_code.clone()),
            constants: std::sync::Arc::new(self.program_constants.clone()),
            strings: std::sync::Arc::new(self.program_strings.clone()),
            static_slot_count: self.statics.len() as u32,
            debug: self.program_debug.clone(),
            operand_stack_slots: self.stack.capacity() as u32,
        }));
    }

    /// Register a function signature on a previously-loaded
    /// userland library (host/test helper — userland code uses
    /// `DeclareFFI` at runtime).
    pub fn register_ffi_function(
        &mut self,
        library_value: Value,
        signature: crate::ffi::FfiSignature,
    ) -> Result<usize, String> {
        let addr = library_value.raw() as u64;
        let mut lib_obj_arc = self
            .userland_libraries
            .get(&addr)
            .cloned()
            .ok_or_else(|| format!("not a loaded library: 0x{:x}", addr))?;
        let lib_obj_mut = std::sync::Arc::make_mut(&mut lib_obj_arc);
        if let crate::memory::Object::Library(gc) = lib_obj_mut {
            let obj_lib: &mut crate::memory::ObjLibrary = (**gc).as_mut();
            let id = crate::ffi::register_on_library(obj_lib, signature, &self.struct_layouts)
                .map_err(|e| e.to_string())?;
            self.userland_libraries
                .insert(addr, std::sync::Arc::new(*lib_obj_mut));
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

    fn with_coroutine_mut(&self, addr: u64, f: impl FnOnce(&mut ObjCoroutine)) {
        let mut current = self.heap.head_for_lookup();
        while let Some(reference) = current {
            if reference.addr() == addr {
                if let Object::Coroutine(gc) = reference {
                    f(gc.payload_mut());
                }
                return;
            }
            current = reference.get_next();
        }
    }

    fn find_delegator(&self, sub: RefCoroutine) -> Option<RefCoroutine> {
        let sub_addr = sub.as_ptr() as u64;
        let mut current = self.heap.head_for_lookup();
        while let Some(reference) = current {
            if let Object::Coroutine(gc) = reference {
                if gc
                    .as_ref()
                    .yield_from
                    .as_ref()
                    .is_some_and(|d| d.as_ptr() as u64 == sub_addr)
                {
                    return Some(gc.clone());
                }
            }
            current = reference.get_next();
        }
        None
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
        self.with_coroutine_mut(coro_gc.as_ptr() as u64, |coro| {
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
        if unlikely(!self.resume_stack.is_empty())
            && let Some(ctx) = self.resume_stack.last()
            && self.frames.len() <= ctx.frame_depth
        {
            let coro_ptr = ctx.coro.as_ptr() as u64;
            let old_wait = {
                let mut taken = None;
                self.with_coroutine_mut(coro_ptr, |coro| {
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
    ) {
        let token = self
            .io_reactor
            .register_wait(req.handle, req.interest);
        let coro_ptr = self
            .resume_stack
            .last()
            .expect("cooperative await requires an active coroutine")
            .coro
            .as_ptr() as u64;
        let old = {
            let mut taken = None;
            self.with_coroutine_mut(coro_ptr, |c| {
                taken = c.io_wait.replace(token);
            });
            taken
        };
        if let Some(old) = old {
            self.io_reactor.cancel_wait(old);
        }
        let ok = crate::io::as_result_unit(&mut self.heap, Ok(()));
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
    ) {
        let return_ip = *ip;
        let coro = gc.as_ref();
        let base_sp = self.stack.tell();

        self.frames.get_mut().seek(return_ip);

        let old_wait = {
            let mut taken = None;
            self.with_coroutine_mut(gc.as_ptr() as u64, |c| {
                taken = c.io_wait.take();
                c.pending_send = send_val;
            });
            taken
        };
        if let Some(tok) = old_wait {
            self.io_reactor.cancel_wait(tok);
        }

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
        self.with_coroutine_mut(coro_gc.as_ptr() as u64, |coro| {
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
    ) {
        let Some(outer_ctx) = self.resume_stack.last().copied() else {
            return;
        };
        let outer = outer_ctx.coro;
        self.save_coroutine_state(outer, *ip, *sp, outer_ctx.base_sp, outer_ctx.frame_depth);
        self.with_coroutine_mut(outer.as_ptr() as u64, |outer_coro| {
            outer_coro.yield_from = Some(sub);
            outer_coro.yield_from_resume_ip = *ip;
        });
        self.resume_coroutine(ip, sp, sub, Value::from(0_i64), code, false);
    }

    /// Read-only access to the heap. Used by the GC integration
    /// test to assert that the heap didn't grow unboundedly.
    pub fn heap(&self) -> &Heap {
        &self.heap
    }

    /// True when a language-level `panic` aborted the last run.
    pub fn panicked(&self) -> bool {
        self.panicked
    }

    /// Load bytecode for reentrant [`call_function`] without running `main`.
    pub fn load_program(&mut self, code: &[RawByte], constants: &[u64], strings: &[String]) {
        self.program_code = code.to_vec();
        self.program_constants = constants.to_vec();
        self.install_program_strings(strings);
        self.panicked = false;
    }

    /// Rewrite the first `JMP target` at or after `from` into `HALT` so setup
    /// can run without falling through into `main`.
    pub fn halt_first_jump_to(&mut self, from: usize, target: u32) {
        let code: &mut [Byte] = unsafe {
            std::slice::from_raw_parts_mut(self.program_code.as_mut_ptr().cast(), self.program_code.len())
        };
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
            let paused = self.execute(code, constants, ip);
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
        self.program_code = unsafe {
            std::slice::from_raw_parts(code.as_ptr().cast::<RawByte>(), code.len()).to_vec()
        };
        self.program_constants = constants.to_vec();
        self.install_program_strings(strings);
        self.sync_thread_program_from_current();
        let mut ip = 0usize;
        loop {
            let paused = self.execute(code, constants, ip);
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
        // forever unless told to stop — otherwise every program that spawns
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
        let v = crate::io::as_result_unit(&mut self.heap, wait);
        self.stack.push(v);
    }

    fn finish_pending_ffi_invoke(&mut self, pending: PendingFfiInvoke) {
        self.frames.get_mut().set(pending.resume_sp);
        let lib_obj = self.userland_libraries.get(&pending.lib_addr).cloned();
        let invoke_result = match lib_obj {
            Some(obj) => {
                let l = match obj.as_ref() {
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
                        &mut self.heap as *mut Heap,
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

    /// Call a coil function at `offset` reentrantly (for FFI callbacks).
    pub fn call_function(&mut self, offset: u32, args: &[Value]) -> Value {
        let saved_sp = self.stack.tell();
        for a in args {
            self.stack.push(*a);
        }
        self.nested_return = None;
        self.nested_depth += 1;
        let callee_sp = self.stack.tell().saturating_sub(args.len());
        self.frames.setup_current_and_advance(|f| {
            f.seek(0);
            f.set(callee_sp);
        });
        // Capture only when RETURN reaches this frame depth (the
        // call_function entry), not when inner CALLs return.
        self.nested_frame_depths.push(self.frames.len());
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
        let mut ip = offset as usize;
        loop {
            let paused = self.execute(code, constants, ip);
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
        self.nested_return.take().unwrap_or_default()
    }

    /// Stash a return value when `execute` runs inside [`Self::call_function`].
    #[inline]
    fn capture_nested_return(&mut self, ret_val: Value) -> bool {
        // Nested FFI/host calls are rare; keep the hot RETURN path branch-free.
        if unlikely(self.nested_depth > 0) {
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
    /// Prefetch + fused jump tables live *inside* this outlined copy.
    #[inline(never)]
    fn execute(&mut self, code: &[Byte], constants: &[u64], start_ip: usize) -> bool {
        let _active_guard = crate::thread::HostStateGuard::enter(self);

        let mut ip: usize = start_ip;
        let mut sp = self.frames.get_mut().get();
        let stack_cap = self.stack.capacity();
        let code_len = code.len();

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

            #[cfg(any(test, feature = "vm_profile"))]
            VM_DISPATCH_COUNT.with(|c| c.fetch_add(1, Ordering::Relaxed));

            #[cfg(any(test, feature = "vm_profile"))]
            VM_CURSOR_TRACE.with(|t| {
                let mut t = t.borrow_mut();
                if t.len() < CURSOR_TRACE_CAP {
                    t.push((ip as u32, (self.stack.tell().saturating_sub(sp)) as u32));
                }
            });

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
            promise!(*bc as u8 <= Instruction::StoreIndexPinUnchecked as u8);

            match bc {
                Instruction::POP => {
                    self.stack.pop();
                }
                Instruction::DUPLICATE => {
                    self.stack.duplicate();
                }
                Instruction::CONST => {
                    let op = opcode.operand_u32();
                    let raw = if unlikely(op & Byte::POOL_FLAG != 0) {
                        let pool_idx = (op & !Byte::POOL_FLAG) as usize;
                        promise!(pool_idx < constants.len());
                        unsafe { *constants.get_unchecked(pool_idx) }
                    } else {
                        op as i32 as i64 as u64
                    };
                    self.stack.push(Value::from(raw));
                }
                Instruction::CodePtr => {
                    // Absolute bytecode entry — same stack representation as an
                    // integer constant so `CallIndirect` / dict `Index` can
                    // treat it as a raw code offset.
                    let offset = opcode.operand_u32() as i64;
                    self.stack.push(Value::from(offset));
                }
                Instruction::STORE => {
                    // Pop TOS into each listed slot (packed n=1..=3, or wide n=0).
                    // After all pops, keep the shared operand/local cursor at or
                    // past the highest written slot so later pushes do not
                    // clobber multi-slot locals (fixed `[T; N]` on stack).
                    let count = opcode.load_store_count();
                    let mut max_slot = sp;
                    for i in 0..count {
                        let slot = sp + opcode.load_store_slot_at(i) as usize;
                        promise!(slot < stack_cap);
                        max_slot = max_slot.max(slot);
                        let val = self.stack.pop();
                        self.stack[slot] = val;
                    }
                    let need = max_slot + 1;
                    if self.stack.tell() < need {
                        self.stack.seek(need);
                    }
                }
                Instruction::Seek => {
                    // Frame-relative cursor: operands[31:0] = slot offset from `sp`.
                    let slot = opcode.operand_u32() as usize;
                    let abs = sp + slot;
                    promise!(abs <= stack_cap);
                    self.stack.seek(abs);
                }
                Instruction::OptionNicheToHeap => {
                    return self.runtime_panic(
                        "retired opcode OptionNicheToHeap",
                        ip.saturating_sub(1),
                    );
                }
                Instruction::HeapOptionToNiche => {
                    return self.runtime_panic(
                        "retired opcode HeapOptionToNiche",
                        ip.saturating_sub(1),
                    );
                }
                Instruction::PairJumpIfTag => {
                    return self.runtime_panic(
                        "retired opcode PairJumpIfTag",
                        ip.saturating_sub(1),
                    );
                }
                Instruction::PairToHeap => {
                    return self.runtime_panic("retired opcode PairToHeap", ip.saturating_sub(1));
                }
                Instruction::HeapToPair => {
                    return self.runtime_panic("retired opcode HeapToPair", ip.saturating_sub(1));
                }
                Instruction::ReturnPair => {
                    return self.runtime_panic("retired opcode ReturnPair", ip.saturating_sub(1));
                }
                Instruction::LOAD => {
                    let count = opcode.load_store_count();
                    for i in 0..count {
                        let slot = opcode.load_store_slot_at(i) as usize;
                        promise!(sp + slot < stack_cap);
                        self.stack.push(self.stack[sp + slot]);
                    }
                }
                Instruction::INC => {
                    let (slot, prefix, is_float) = opcode.inc_dec_parts();
                    promise!(sp + slot < stack_cap);
                    let idx = sp + slot;
                    let old = self.stack[idx];
                    let new_val = if is_float {
                        Value::from(old.as_float() + 1.0)
                    } else {
                        Value::from(old.as_int() + 1)
                    };
                    self.stack[idx] = new_val;
                    self.stack.push(if prefix { new_val } else { old });
                }
                Instruction::DEC => {
                    let (slot, prefix, is_float) = opcode.inc_dec_parts();
                    promise!(sp + slot < stack_cap);
                    let idx = sp + slot;
                    let old = self.stack[idx];
                    let new_val = if is_float {
                        Value::from(old.as_float() - 1.0)
                    } else {
                        Value::from(old.as_int() - 1)
                    };
                    self.stack[idx] = new_val;
                    self.stack.push(if prefix { new_val } else { old });
                }
                Instruction::NOT => unary!(self.stack, !, as_int),
                Instruction::LogNot => {
                    let val = self.stack.pop();
                    self.stack.push(Value::from(!(val.as_int() != 0)));
                }
                Instruction::NEG => unary!(self.stack, -, as_int),
                // IEEE negate: flip sign bit (preserves NaN payload).
                Instruction::NEGF => {
                    let sp = self.stack.tell();
                    promise!(sp >= 1);
                    let idx = sp - 1;
                    let bits = self.stack[idx].raw() as u64;
                    self.stack[idx].replace((bits ^ (1u64 << 63)) as _);
                }
                Instruction::AND => binary!(self.stack, &&, as_bool),
                Instruction::OR => binary!(self.stack, ||, as_bool),
                Instruction::ADD => binary!(self.stack, +, as_int),
                Instruction::SUB => binary!(self.stack, -, as_int),
                Instruction::MUL => binary!(self.stack, *, as_int),
                Instruction::DIV => binary!(self.stack, /, as_int),
                Instruction::MOD => binary!(self.stack, %, as_int),
                Instruction::LE => binary!(self.stack, <, as_int),
                Instruction::LEQ => binary!(self.stack, <=, as_int),
                Instruction::GT => binary!(self.stack, >, as_int),
                Instruction::GEQ => binary!(self.stack, >=, as_int),
                Instruction::EQ => {
                    let sp = self.stack.tell();
                    promise!(sp >= 2);
                    let rhs = self.stack[sp - 1];
                    let lhs = self.stack[sp - 2];
                    let eq = crate::value_eq::values_eq(&self.heap, lhs, rhs);
                    self.stack[sp - 2].replace(eq as _);
                    self.stack.seek(sp - 1);
                }
                Instruction::NEQ => {
                    let sp = self.stack.tell();
                    promise!(sp >= 2);
                    let rhs = self.stack[sp - 1];
                    let lhs = self.stack[sp - 2];
                    let eq = crate::value_eq::values_eq(&self.heap, lhs, rhs);
                    self.stack[sp - 2].replace((!eq) as _);
                    self.stack.seek(sp - 1);
                }
                Instruction::ADDF => binary!(self.stack, +, as_float, to_bits),
                Instruction::SUBF => binary!(self.stack, -, as_float, to_bits),
                Instruction::MULF => binary!(self.stack, *, as_float, to_bits),
                Instruction::DIVF => binary!(self.stack, /, as_float, to_bits),
                Instruction::MODF => binary!(self.stack, %, as_float, to_bits),
                Instruction::SHL => binary!(self.stack, <<, as_int),
                Instruction::SHR => binary!(self.stack, >>, as_int),
                Instruction::XOR => binary!(self.stack, ^, as_int),
                Instruction::BITAND => binary!(self.stack, &, as_int),
                Instruction::BITOR => binary!(self.stack, |, as_int),
                Instruction::Pow => {
                    let sp = self.stack.tell();
                    promise!(sp >= 2);
                    let rhs = self.stack[sp - 1].as_int();
                    let lhs = self.stack[sp - 2].as_int();
                    let result = lhs.pow(rhs as u32);
                    self.stack[sp - 2].replace(result as _);
                    self.stack.seek(sp - 1);
                }
                Instruction::PowF => {
                    let sp = self.stack.tell();
                    promise!(sp >= 2);
                    let rhs = self.stack[sp - 1].as_float();
                    let lhs = self.stack[sp - 2].as_float();
                    let result = lhs.powf(rhs);
                    self.stack[sp - 2].replace(result.to_bits() as _);
                    self.stack.seek(sp - 1);
                }
                Instruction::LEF => binary!(self.stack, <, as_float),
                Instruction::LEQF => binary!(self.stack, <=, as_float),
                Instruction::GTF => binary!(self.stack, >, as_float),
                Instruction::GEQF => binary!(self.stack, >=, as_float),
                Instruction::FORMAT => {
                    let params_count = opcode.operand_u32();
                    if params_count != 0 {
                        let mut params = ArrayVec::<Value, 8>::default();

                        for _ in 0..params_count as usize {
                            params.push(self.stack.pop());
                        }

                        let ptr = self.stack.pop().as_ptr::<GcData<ObjString>>();
                        let format_string = (unsafe { &*ptr }).as_ref().data.as_str();

                        let mut message = String::default();

                        let mut chars = format_string.chars().peekable();
                        while let Some(ch) = chars.next() {
                            if ch == '%' {
                                match chars.peek() {
                                    Some('i') => {
                                        chars.next();
                                        message.push_str(&params.pop().as_int().to_string());
                                    }
                                    Some('f') => {
                                        chars.next();
                                        // message
                                        //     .push_str(&format!("{:.?}", params.pop().as_float()));
                                        let _ =
                                            write!(&mut message, "{:.?}", params.pop().as_float());
                                    }
                                    Some('b') => {
                                        chars.next();
                                        let _ = write!(
                                            &mut message,
                                            "{:0b}",
                                            params.pop().raw().addr()
                                        );
                                    }
                                    Some('s') => {
                                        chars.next();
                                        let string_val =
                                            (unsafe { &*params.pop().as_ptr::<GcData<ObjString>>() })
                                                .as_ref()
                                                .data
                                                .as_str();
                                        // Allocated::<crate::String>::new(params.pop().as_ptr());
                                        message.push_str(string_val);
                                    }
                                    Some('x') => {
                                        chars.next();
                                        let _ = write!(
                                            &mut message,
                                            "{:0x}",
                                            params.pop().raw().addr()
                                        );
                                    }
                                    Some('z') => {
                                        chars.next();
                                        message.push_str(if params.pop().raw() > 0 as _ {
                                            "true"
                                        } else {
                                            "false"
                                        });
                                    }
                                    Some('u') => {
                                        chars.next();
                                        message.push_str(&params.pop().raw().addr().to_string());
                                    }
                                    Some('p') => {
                                        chars.next();
                                        let _ = write!(
                                            &mut message,
                                            "{:08x}",
                                            params.pop().as_ptr::<bool>().addr()
                                        );
                                    }
                                    _ => {
                                        message.push('%');
                                    }
                                }
                            } else {
                                message.push(ch);
                            }
                        }

                        self.push_interned_string(message);
                    }
                }
                Instruction::STRINGIFY => {
                    // Shared primitive conversion for Show thunks / `%v`.
                    // Accepts a boxed value (preferred), a heap string, or a
                    // raw immediate (treated as int).
                    let v = self.stack.pop();
                    let text = Self::stringify_value(&self.heap, v);
                    self.push_interned_string(text);
                }
                Instruction::PRINT => {
                    let ptr = self.stack.pop().as_ptr::<GcData<ObjString>>();
                    let s = unsafe { (*ptr).as_ref() };
                    if let Some(out) = self.output.as_mut() {
                        let _ = write!(out, "{}", s);
                        let _ = out.flush();
                    } else {
                        print!("{}", s);
                        let _ = io::stdout().flush();
                    }
                }
                Instruction::JMP => {
                    set_jump_target(&mut ip, opcode.operand_u32() as usize, code);
                }
                Instruction::JMPF => {
                    if !self.stack.pop().as_bool() {
                        set_jump_target(&mut ip, opcode.operand_u32() as usize, code);
                    }
                }
                Instruction::JMPT => {
                    if self.stack.pop().as_bool() {
                        set_jump_target(&mut ip, opcode.operand_u32() as usize, code);
                    }
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
                    if likely(target != 0) {
                        self.frames.rewrite_top_and_push(
                            |caller| caller.seek(ip),
                            |frame| frame.set(callee_sp),
                        );
                        sp = callee_sp;
                        set_jump_target(&mut ip, target, code);
                    } else {
                        self.frames.rewrite_top_and_push(
                            |caller| caller.seek(ip + 1),
                            |frame| frame.set(callee_sp),
                        );
                        sp = callee_sp;
                    }
                }
                Instruction::TailCall => {
                    let (arity, target) = opcode.call_parts();
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
                Instruction::CastIntToFloat => {
                    let v = self.stack.pop().as_int() as f64;
                    self.stack.push(Value::from(v));
                }
                Instruction::CastFloatToInt => {
                    // Truncate toward zero (`3.9 as int` → `3`); not floor/round.
                    let v = self.stack.pop().as_float() as i64;
                    self.stack.push(Value::from(v));
                }
                Instruction::CastIntToByte => {
                    let v = self.stack.pop().as_int();
                    self.stack.push(Value::from((v as u8) as i64));
                }
                Instruction::CastByteToInt => {
                    let v = self.stack.pop().as_int();
                    self.stack.push(Value::from(v & 0xff));
                }
                Instruction::CastIntToBool => {
                    let v = self.stack.pop().as_int();
                    self.stack.push(Value::from((v != 0) as i64));
                }
                Instruction::CastBoolToInt => {
                    let v = self.stack.pop().as_int();
                    self.stack.push(Value::from(if v != 0 { 1 } else { 0 }));
                }
                Instruction::INIT => {
                    let (_, mut r) = self.heap.alloc(ObjInstance::default(), Object::Instance);
                    let _ = r.as_mut();
                    // Root before GC — same rule as `push_interned_string`.
                    self.stack.push(Value::from(r.as_ptr().addr() as u64));
                    self.maybe_gc_after_alloc();
                }
                Instruction::InitTyped => {
                    let (type_id, nfields) = unpack_init_typed(opcode.operand_u32());
                    let (_, mut r) = self.heap.alloc(
                        ObjInstance::with_type_id_and_fields(type_id, nfields as usize),
                        Object::Instance,
                    );
                    let _ = r.as_mut();
                    self.stack.push(Value::from(r.as_ptr().addr() as u64));
                    self.maybe_gc_after_alloc();
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
                // Fused `LOAD slot; CONST imm; <binop>` — compute in place
                // (same shape as `BinSlotSlot`) to avoid two temp pushes.
                Instruction::BinSlotImm => {
                    let (op, slot, imm) = opcode.bin_slot_imm_parts();
                    promise!(sp + slot < stack_cap);
                    let lhs = self.stack[sp + slot];
                    let rhs = Value::from(imm);
                    let result = crate::fused::eval_bin(op, lhs, rhs, &self.heap);
                    self.stack.push(result);
                }
                // Fused `<cmp|cond>; JMPF/JMPT target`.
                Instruction::CmpJmpf | Instruction::CmpJmpt => {
                    let (op, t) = opcode.cmp_jmpf_parts();
                    let target = if opcode.cmp_jmpf_is_pool() {
                        promise!(t < constants.len());
                        unsafe { *constants.get_unchecked(t) as usize }
                    } else {
                        t
                    };
                    let tos = self.stack.tell();
                    promise!(tos >= 2);
                    let rhs = self.stack[tos - 1];
                    let lhs = self.stack[tos - 2];
                    self.stack.seek(tos - 2);
                    let taken = crate::fused::eval_cmp(op, lhs, rhs, &self.heap);
                    if taken == matches!(*bc, Instruction::CmpJmpt) {
                        set_jump_target(&mut ip, target, code);
                    }
                }
                // Fused `LOAD slot; CONST imm; <cond>; JMPF/JMPT` without stack traffic.
                Instruction::BinSlotImmJmpf | Instruction::BinSlotImmJmpt => {
                    let (op, slot, pool_idx) = opcode.bin_slot_imm_jmpf_parts();
                    promise!(pool_idx < constants.len());
                    let packed = unsafe { *constants.get_unchecked(pool_idx) };
                    let imm = packed as u32 as i32 as i64;
                    let target = (packed >> 32) as usize;
                    promise!(sp + slot < stack_cap);
                    let lhs = self.stack[sp + slot];
                    let rhs = Value::from(imm);
                    let taken = crate::fused::eval_cmp(op, lhs, rhs, &self.heap);
                    if taken == matches!(*bc, Instruction::BinSlotImmJmpt) {
                        set_jump_target(&mut ip, target, code);
                    }
                }
                Instruction::LogNotJmpf | Instruction::LogNotJmpt => {
                    let t = opcode.log_not_jmpf_target();
                    let target = if opcode.log_not_jmpf_is_pool() {
                        promise!(t < constants.len());
                        unsafe { *constants.get_unchecked(t) as usize }
                    } else {
                        t
                    };
                    let val = self.stack.pop();
                    if (val.as_int() == 0) == matches!(*bc, Instruction::LogNotJmpt) {
                        set_jump_target(&mut ip, target, code);
                    }
                }
                // Fused `BinSlotSlot; JMPF/JMPT` — pool packs (target<<32)|b.
                Instruction::BinSlotSlotJmpf | Instruction::BinSlotSlotJmpt => {
                    let (op, a, pool_idx) = opcode.bin_slot_slot_jmpf_parts();
                    promise!(pool_idx < constants.len());
                    let packed = unsafe { *constants.get_unchecked(pool_idx) };
                    let b = (packed as u32 & 0xFF) as usize;
                    let target = (packed >> 32) as usize;
                    promise!(sp + a < stack_cap);
                    promise!(sp + b < stack_cap);
                    let va = self.stack[sp + a];
                    let vb = self.stack[sp + b];
                    let taken = crate::fused::eval_cmp(op, va, vb, &self.heap);
                    if taken == matches!(*bc, Instruction::BinSlotSlotJmpt) {
                        set_jump_target(&mut ip, target, code);
                    }
                }
                // Fused `LOAD src; CONST imm; <op>; STORE dest` — pool packs (dest<<32)|imm.
                Instruction::BinSlotImmStore => {
                    let (op, slot, pool_idx) = opcode.bin_slot_imm_store_parts();
                    promise!(pool_idx < constants.len());
                    let packed = unsafe { *constants.get_unchecked(pool_idx) };
                    let imm = packed as u32 as i32 as i64;
                    let dest = (packed >> 32) as usize;
                    promise!(sp + slot < stack_cap);
                    let lhs = self.stack[sp + slot];
                    let rhs = Value::from(imm);
                    let result = crate::fused::eval_bin(op, lhs, rhs, &self.heap);
                    let dest_idx = sp + dest;
                    promise!(dest_idx < stack_cap);
                    self.stack[dest_idx] = result;
                    let tell = self.stack.tell();
                    if tell < dest_idx + 1 {
                        self.stack.seek(dest_idx + 1);
                    }
                }
                // Fused `LOAD a; LOAD b; <op>; STORE dest`.
                Instruction::BinSlotSlotStore => {
                    let (op, a, b, dest) = opcode.bin_slot_slot_store_parts();
                    promise!(sp + a < stack_cap);
                    promise!(sp + b < stack_cap);
                    promise!(sp + dest < stack_cap);
                    let va = self.stack[sp + a];
                    let vb = self.stack[sp + b];
                    let result = crate::fused::eval_bin(op, va, vb, &self.heap);
                    let dest_idx = sp + dest;
                    self.stack[dest_idx] = result;
                    let tell = self.stack.tell();
                    if tell < dest_idx + 1 {
                        self.stack.seek(dest_idx + 1);
                    }
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
                    let ret_val = crate::fused::eval_bin(opcode.bin_return_op(), lhs, rhs, &self.heap);
                    if self.capture_nested_return(ret_val) {
                        return false;
                    }
                    let return_sp = self.pop_call_frame();
                    self.stack.seek(return_sp);
                    self.stack.push(ret_val);
                    self.after_return(&mut ip, &mut sp);
                }
                Instruction::BinSlotSlot => {
                    let (op, a, b) = opcode.bin_slot_slot_parts();
                    promise!(sp + a < stack_cap);
                    promise!(sp + b < stack_cap);
                    let va = self.stack[sp + a];
                    let vb = self.stack[sp + b];
                    let result = crate::fused::eval_bin(op, va, vb, &self.heap);
                    self.stack.push(result);
                }
                Instruction::NATIVE => {
                    #[cfg(debug_assertions)]
                    eprintln!("FFI: deprecated NATIVE opcode — recompile from source");
                }
                Instruction::FfiLoad => {
                    // Inlined to split-borrow `heap`/`libraries` from `frames`.
                    let path_val = self.stack.pop();
                    let path = {
                        let addr = path_val.raw() as u64;
                        match Self::find_object_by_addr(&self.heap, addr) {
                            Some(crate::memory::Object::String(gc)) => gc.as_ref().data.clone(),
                            _ => String::new(),
                        }
                    };
                    // Push `Result::Ok(handle)` or `Result::Err(ffi::Error)`.
                    match crate::ffi::resolve_library(
                        &path,
                        self.base_dir.as_deref(),
                        &self.ffi_search_paths,
                        &self.dload_gate,
                    ) {
                        Ok(lib_arc) => {
                            self.libraries
                                .entry(path.clone())
                                .or_insert_with(|| lib_arc.clone());
                            let (object, _gc) = self.heap.alloc_library(lib_arc);
                            let addr = object.addr();
                            self.userland_libraries
                                .insert(addr, std::sync::Arc::new(object));
                            self.push_result_ok(Value::from(addr as *mut u8));
                        }
                        Err(e) => {
                            self.push_ffi_error(e);
                        }
                    }
                }
                Instruction::FfiInvoke => {
                    let raw = opcode.operand_u32();
                    let _arity = (raw & 0xFFFF) as usize;
                    let has_arg_tags = (raw & (1 << 16)) != 0;

                    // Stack (bottom → top): lib, fn_id, args_tuple [, tags_tuple].
                    let arg_types = if has_arg_tags {
                        let tags_val = self.stack.pop();
                        let tags_addr = tags_val.raw() as u64;
                        let tags: Vec<crate::memory::FfiType> =
                            match Self::find_object_by_addr(&self.heap, tags_addr) {
                                Some(crate::memory::Object::Tuple(gc)) => gc
                                    .as_ref()
                                    .elements
                                    .iter()
                                    .map(|v| Self::ffi_type_from_value(v, &self.heap))
                                    .collect(),
                                _ => Vec::new(),
                            };
                        Some(tags)
                    } else {
                        None
                    };

                    let tuple_val = self.stack.pop();
                    let tuple_addr = tuple_val.raw() as u64;

                    let function_id_val = self.stack.pop();
                    let function_id = function_id_val.as_int() as usize;

                    let lib_val = self.stack.pop();
                    let lib_addr = lib_val.raw() as u64;

                    let args: Vec<Value> = match Self::find_object_by_addr(&self.heap, tuple_addr) {
                        Some(crate::memory::Object::Tuple(gc)) => gc.as_ref().elements.clone(),
                        _ => Vec::new(),
                    };

                    self.frames.get_mut().set(sp);
                    self.pending_ffi = Some(PendingFfiInvoke {
                        lib_addr,
                        function_id,
                        args,
                        arg_types,
                        resume_ip: ip,
                        resume_sp: sp,
                    });
                    return true;
                }
                Instruction::DeclareFFI => {
                    let raw = opcode.operand_u32();
                    let _arity = (raw & 0xFFFF) as usize;
                    let variadic = (raw & (1 << 16)) != 0;

                    // Stack (bottom → top): lib, name, args_tuple, ret_tag.
                    let ret_tag_val = self.stack.pop();
                    let ret_type = Self::ffi_type_from_value(&ret_tag_val, &self.heap);

                    // Pop the args tuple (next on the stack).
                    let args_tuple_val = self.stack.pop();
                    let args_tuple_addr = args_tuple_val.raw() as u64;

                    let arg_types: Vec<crate::memory::FfiType> =
                        match Self::find_object_by_addr(&self.heap, args_tuple_addr) {
                            Some(crate::memory::Object::Tuple(gc)) => gc
                                .as_ref()
                                .elements
                                .iter()
                                .map(|v| Self::ffi_type_from_value(v, &self.heap))
                                .collect(),
                            _ => Vec::new(),
                        };
                    // Pop the name string.
                    let name_val = self.stack.pop();
                    let name = Self::object_string_value(&self.heap, &name_val);
                    // Pop the lib handle.
                    let lib_val = self.stack.pop();
                    let lib_addr = lib_val.raw() as u64;
                    let lib_obj = self.userland_libraries.get(&lib_addr).cloned();
                    match lib_obj {
                        Some(obj_arc) => {
                            let mut owned = *obj_arc;
                            let ffi_sig = crate::ffi::FfiSignature {
                                name,
                                args: arg_types,
                                ret: ret_type,
                                variadic,
                            };
                            match Self::register_signature_on_object(
                                &mut owned,
                                ffi_sig,
                                &self.struct_layouts,
                            ) {
                                Ok(id) => {
                                    self.userland_libraries
                                        .insert(lib_addr, std::sync::Arc::new(owned));
                                    self.push_result_ok(Value::from(id as i64));
                                }
                                Err(e) => {
                                    self.push_ffi_error(e);
                                }
                            }
                        }
                        None => {
                            self.push_result_err(
                                crate::ffi::FfiErrorKindTag::InvalidHandle,
                                format!("FFI declare: library at 0x{:x} is not loaded", lib_addr),
                            );
                        }
                    }
                }
                Instruction::HostInvoke => {
                    let arity = (opcode.operand_u32() & 0xFFFF) as usize;
                    let tell = self.stack.tell();
                    let consume = arity + 1;
                    promise!(tell >= consume);
                    let fn_id = self.stack.top_window(consume)[0].as_int() as usize;
                    // Packed LA (and other host natives) allocate via
                    // `heap.alloc` inside the closure; count those so GC
                    // pressure still fires when HostInvoke is the only
                    // allocator on a hot path.
                    let live_before = self.heap.live_object_count();
                    let host_op = match self.natives.get_by_id(fn_id) {
                        Some(native) => native.host_op(),
                        None => {
                            return self.runtime_panic(
                                &format!("HostInvoke: unknown native id {fn_id}"),
                                ip.saturating_sub(1),
                            );
                        }
                    };
                    match host_op {
                        crate::HostOp::Collect => {
                            self.stack.seek(tell - consume);
                            let before = self.heap.size();
                            self.gc_collect();
                            let freed = before.saturating_sub(self.heap.size());
                            self.stack.push(Value::from(freed as i64));
                        }
                        crate::HostOp::RegisterFinalizer => {
                            let args = self.stack.top_window(consume);
                            let type_id = args.get(1).map(|v| v.as_int() as u32).unwrap_or(0);
                            let pc = args.get(2).map(|v| v.as_int() as u32).unwrap_or(0);
                            self.register_finalizer(type_id, pc);
                            self.stack.seek(tell - consume);
                            self.stack.push(Value::from(0i64));
                        }
                        crate::HostOp::Ordinary => {
                            let native = self
                                .natives
                                .get_by_id(fn_id)
                                .expect("id checked above");
                            let args = &self.stack.top_window(consume)[1..];
                            match native.invoke(&mut self.heap, args) {
                                Ok(Some(v)) => {
                                    self.stack.seek(tell - consume);
                                    self.stack.push(v);
                                }
                                Ok(None) => {
                                    self.stack.seek(tell - consume);
                                    if let Some(req) = crate::io::take_pending_io_park() {
                                        if !self.resume_stack.is_empty() {
                                            // Inside a coroutine: register for batch
                                            // poll and yield (do not park the VM).
                                            self.cooperative_io_await_yield(
                                                &mut ip, &mut sp, req,
                                            );
                                        } else {
                                            self.frames.get_mut().set(sp);
                                            self.pending_io = Some(PendingIoWait {
                                                request: req,
                                                resume_ip: ip,
                                                resume_sp: sp,
                                            });
                                            return true;
                                        }
                                    } else {
                                        // Void natives must still leave a defined TOS.
                                        self.stack.push(Value::default());
                                    }
                                }
                                Err(e) => {
                                    let name = native.name();
                                    return self.runtime_panic(
                                        &format!("HostInvoke failed for `{name}`: {e}"),
                                        ip.saturating_sub(1),
                                    );
                                }
                            }
                        }
                    }
                    let allocated = self.heap.live_object_count().saturating_sub(live_before);
                    if allocated > 0 {
                        self.maybe_gc_after_alloc();
                    }
                }
                Instruction::HostInvokeNiche => {
                    return self.runtime_panic(
                        "retired opcode HostInvokeNiche",
                        ip.saturating_sub(1),
                    );
                }
                Instruction::FloatChainStore => {
                    return self.runtime_panic(
                        "retired opcode FloatChainStore",
                        ip.saturating_sub(1),
                    );
                }
                // Fused `BinSlotSlot <arith>; CONST pool; CmpJmpf/CmpJmpt` — no stack traffic.
                Instruction::BinSlotSlotConstJmpf => {
                    return self.runtime_panic(
                        "retired opcode BinSlotSlotConstJmpf",
                        ip.saturating_sub(1),
                    );
                }
                Instruction::BinSlotSlotConstJmpt => {
                    let (bin_op, a, desc_idx) = opcode.bin_slot_slot_const_jmpf_parts();
                    promise!(desc_idx < constants.len());
                    let packed = unsafe { *constants.get_unchecked(desc_idx) };
                    let (b, cmp_op, float_idx, target) =
                        RawByte::unpack_bin_slot_slot_const_jmpf_desc(packed);
                    let b = b as usize;
                    promise!(float_idx < constants.len());
                    promise!(sp + a < stack_cap);
                    promise!(sp + b < stack_cap);
                    let va = self.stack[sp + a].as_float();
                    let vb = self.stack[sp + b].as_float();
                    let mag = crate::fused::eval_f64_bin(bin_op, va, vb);
                    let rhs = Value::from(unsafe { *constants.get_unchecked(float_idx) }).as_float();
                    let taken = crate::fused::eval_f64_cmp(cmp_op, mag, rhs);
                    if taken == matches!(*bc, Instruction::BinSlotSlotConstJmpt) {
                        set_jump_target(&mut ip, target, code);
                    }
                }
                Instruction::HALT => {
                    if let Some(out) = self.output.as_mut() {
                        let _ = out.flush();
                    } else {
                        let _ = io::stdout().flush();
                    }
                    return false;
                }
                Instruction::Panic => {
                    let panic_ip = ip.saturating_sub(1);
                    let ptr = self.stack.pop().as_ptr::<GcData<ObjString>>();
                    let s = unsafe { (*ptr).as_ref() };
                    let loc_suffix = self
                        .format_panic_location(panic_ip)
                        .map(|loc| format!(" at {loc}"))
                        .unwrap_or_default();
                    if let Some(out) = self.output.as_mut() {
                        let _ = write!(out, "panic: {}{}", s, loc_suffix);
                        let _ = out.flush();
                    } else {
                        eprint!("panic: {}{}", s, loc_suffix);
                        let _ = io::stderr().flush();
                    }
                    self.panicked = true;
                    return false;
                }
                Instruction::STRING => {
                    let idx = opcode.operand_u32() as usize;
                    promise!(idx < self.program_strings.len());
                    self.push_program_string(idx);
                }
                Instruction::NOOP => continue,
                Instruction::MakeEnum => {
                    // operands: tag (high 16), arity (low 16). Codegen reverse-pushes
                    // args; we read TOS-first into declaration-order payload.
                    // Values stay on the stack until after alloc so GC can root them.
                    let operands = opcode.operand_u32();
                    let tag = operands >> 16;
                    let arity = (operands & 0xFFFF) as usize;

                    if arity == 0 {
                        let object = self.heap.immortal_unit_enum(tag);
                        self.stack.push(Value::from(object.addr()));
                        // No alloc pressure — singleton is immortal.
                        continue;
                    }

                    let sp = self.stack.tell();
                    promise!(sp >= arity);
                    let n = arity;
                    if n <= 3 {
                        note_make_fast();
                    }
                    let payload = Self::stack_copy_enum_payload(&self.heap, &self.stack, sp, n);
                    let obj_enum = ObjEnum { tag, payload };
                    let (object, _) = self.heap.alloc(obj_enum, Object::Enum);
                    // Drop args, then root the fresh enum before maybe-GC.
                    self.stack.seek(sp - n);
                    self.stack.push(Value::from(object.addr()));
                    self.maybe_gc_after_alloc();
                }
                Instruction::MakeTuple | Instruction::MakeArray => {
                    let operands = opcode.operand_u32();
                    let arity = (operands & 0xFFFF) as usize;
                    let sp = self.stack.tell();
                    promise!(sp >= arity);
                    let n = arity;
                    let base = sp - n;
                    if n <= 3 {
                        note_make_fast();
                    }
                    // Declaration order; keep args on stack through alloc for rooting.
                    let values = Self::stack_copy_decl(&self.stack, base, n);
                    let addr = if matches!(opcode.bytecode(), Instruction::MakeTuple) {
                        let (object, _) = self.heap.alloc(
                            ObjTuple { elements: values },
                            Object::Tuple,
                        );
                        object.addr()
                    } else {
                        let (object, _) = self.heap.alloc(
                            ObjArray { elements: values },
                            Object::Array,
                        );
                        object.addr()
                    };
                    self.stack.seek(base);
                    self.stack.push(Value::from(addr));
                    self.maybe_gc_after_alloc();
                }
                Instruction::ArrayPin => {
                    let slot = opcode.operand_u32();
                    let arr_val = self.stack.pop();
                    let addr = arr_val.raw() as u64;
                    if let Some(Object::Array(gc)) = Self::find_object_by_addr(&self.heap, addr) {
                        self.pin_current_array(slot, Object::Array(gc));
                    }
                }
                Instruction::Index | Instruction::IndexUnchecked => {
                    let index_val = self.stack.pop();
                    let target_val = self.stack.pop();
                    let target_addr = target_val.raw() as u64;
                    let index = index_val.as_int();
                    let unchecked = matches!(*bc, Instruction::IndexUnchecked);
                    // Arrays dominate Index traffic (Vec); check Array before Tuple.
                    let result = match Self::find_object_by_addr(&self.heap, target_addr) {
                        Some(crate::memory::Object::Array(gc)) => {
                            Self::read_indexed(&gc.as_ref().elements, index, unchecked)
                        }
                        Some(crate::memory::Object::Tuple(gc)) => {
                            Self::read_indexed(&gc.as_ref().elements, index, unchecked)
                        }
                        _ => None,
                    };
                    let Some(result) = result else {
                        return self.runtime_panic("index out of bounds", ip.saturating_sub(1));
                    };
                    self.stack.push(result);
                }
                Instruction::IndexPin | Instruction::IndexPinUnchecked => {
                    let slot = opcode.operand_u32();
                    let index = self.stack.pop().as_int();
                    let unchecked = matches!(*bc, Instruction::IndexPinUnchecked);
                    let result = match self.pinned_object(slot) {
                        Some(Object::Array(gc)) => {
                            Self::read_indexed(&gc.as_ref().elements, index, unchecked)
                        }
                        Some(Object::Tuple(gc)) => {
                            Self::read_indexed(&gc.as_ref().elements, index, unchecked)
                        }
                        _ => None,
                    };
                    let Some(result) = result else {
                        return self.runtime_panic("index out of bounds", ip.saturating_sub(1));
                    };
                    self.stack.push(result);
                }
                Instruction::MakeDict => {
                    let arity = (opcode.operand_u32() & 0xFFFF) as usize;
                    let mut pairs: Vec<(crate::memory::RefString, Value)> =
                        Vec::with_capacity(arity);
                    for _ in 0..arity {
                        let name_val = self.stack.pop();
                        let value = self.stack.pop();
                        pairs.push((Self::intern_key(&mut self.heap, name_val), value));
                    }
                    pairs.reverse();
                    // Allocate the instance and populate.
                    let (object, mut gc) =
                        self.heap.alloc(ObjInstance::default(), Object::Instance);
                    {
                        let instance: &mut ObjInstance = gc.as_mut();
                        for (key, value) in pairs {
                            let member = if let Some(obj) =
                                Self::find_object_by_addr(&self.heap, value.raw() as u64)
                            {
                                crate::memory::Member::Object(obj)
                            } else {
                                crate::memory::Member::Value(value)
                            };
                            instance.set(key, member);
                        }
                    }
                    self.stack.push(Value::from(object.addr()));
                    self.maybe_gc_after_alloc();
                }
                Instruction::GetField => {
                    let name_val = self.stack.pop();
                    let target_val = self.stack.pop();
                    let key = Self::intern_key(&mut self.heap, name_val);
                    let target_addr = target_val.raw() as u64;
                    let result = match Self::find_object_by_addr(&self.heap, target_addr) {
                        Some(crate::memory::Object::Instance(gc)) => {
                            match gc.as_ref().get(key) {
                                Some(crate::memory::Member::Value(v)) => v,
                                Some(crate::memory::Member::Object(o)) => Value::from(o.addr()),
                                None => {
                                    return self.runtime_panic(
                                        "no such field",
                                        ip.saturating_sub(1),
                                    );
                                }
                            }
                        }
                        _ => {
                            return self.runtime_panic("no such field", ip.saturating_sub(1));
                        }
                    };
                    self.stack.push(result);
                }
                Instruction::SetField => {
                    if let Some(slot) = set_field_slot_index(opcode.operand_u32()) {
                        let target_val = self.stack.pop();
                        let value = self.stack.pop();
                        let target_addr = target_val.raw() as u64;
                        if let Some(crate::memory::Object::Instance(mut gc)) =
                            Self::find_object_by_addr(&self.heap, target_addr)
                        {
                            let idx = slot as usize;
                            promise!(gc.as_ref().slot_len().is_some_and(|n| idx < n));
                            gc.as_mut()
                                .set_slot(idx, Self::value_as_member(&self.heap, value));
                        } else {
                            return self.runtime_panic(
                                "SetField on non-instance",
                                ip.saturating_sub(1),
                            );
                        }
                        self.stack.push(value);
                    } else {
                        let name_val = self.stack.pop();
                        let target_val = self.stack.pop();
                        let value = self.stack.pop();
                        let key = Self::intern_key(&mut self.heap, name_val);
                        let target_addr = target_val.raw() as u64;
                        if let Some(crate::memory::Object::Instance(mut gc)) =
                            Self::find_object_by_addr(&self.heap, target_addr)
                        {
                            let member = Self::value_as_member(&self.heap, value);
                            gc.as_mut().set(key, member);
                        } else {
                            return self.runtime_panic(
                                "SetField on non-instance",
                                ip.saturating_sub(1),
                            );
                        }
                        self.stack.push(value);
                    }
                }
                Instruction::StoreIndex | Instruction::StoreIndexUnchecked => {
                    let value = self.stack.pop();
                    let index_val = self.stack.pop();
                    let target_val = self.stack.pop();
                    let target_addr = target_val.raw() as u64;
                    let index = index_val.as_int();
                    let unchecked = matches!(*bc, Instruction::StoreIndexUnchecked);
                    if let Some(crate::memory::Object::Array(mut gc)) =
                        Self::find_object_by_addr(&self.heap, target_addr)
                    {
                        let arr = gc.as_mut();
                        let len = arr.elements.len();
                        if unchecked {
                            let idx = index as usize;
                            promise!(index >= 0);
                            promise!(idx < len);
                            unsafe {
                                *arr.elements.get_unchecked_mut(idx) = value;
                            }
                        } else if index >= 0 && (index as usize) < len {
                            unsafe {
                                *arr.elements.get_unchecked_mut(index as usize) = value;
                            }
                        } else {
                            return self
                                .runtime_panic("index out of bounds", ip.saturating_sub(1));
                        }
                    } else {
                        return self.runtime_panic(
                            "StoreIndex on non-array",
                            ip.saturating_sub(1),
                        );
                    }
                    self.stack.push(value);
                }
                Instruction::StoreIndexPin | Instruction::StoreIndexPinUnchecked => {
                    let slot = opcode.operand_u32();
                    let value = self.stack.pop();
                    let index = self.stack.pop().as_int();
                    let unchecked = matches!(*bc, Instruction::StoreIndexPinUnchecked);
                    if let Some(Object::Array(mut gc)) = self.pinned_object(slot) {
                        let arr = gc.as_mut();
                        if !Self::write_indexed(&mut arr.elements, index, value, unchecked) {
                            return self
                                .runtime_panic("index out of bounds", ip.saturating_sub(1));
                        }
                    } else {
                        return self.runtime_panic(
                            "StoreIndexPin on non-array",
                            ip.saturating_sub(1),
                        );
                    }
                    self.stack.push(value);
                }
                Instruction::ArrayPush => {
                    // Stack discipline matches `StoreIndex`: codegen emits
                    // `array` then `value`, so dispatch pops value first,
                    // mutates the heap array in place, and returns the array
                    // address for chaining (`push(push(a, 1), 2)`).
                    let value = self.stack.pop();
                    let target_val = self.stack.pop();
                    let target_addr = target_val.raw() as u64;
                    if let Some(crate::memory::Object::Array(mut gc)) =
                        Self::find_object_by_addr(&self.heap, target_addr)
                    {
                        let old_bytes =
                            gc.as_ref().elements.capacity() * std::mem::size_of::<Value>();
                        gc.as_mut().elements.push(value);
                        let new_bytes =
                            gc.as_ref().elements.capacity() * std::mem::size_of::<Value>();
                        if old_bytes != new_bytes {
                            self.heap.account_resize(old_bytes, new_bytes);
                        }
                    } else {
                        return self
                            .runtime_panic("ArrayPush on non-array", ip.saturating_sub(1));
                    }
                    self.stack.push(target_val);
                }
                Instruction::ArrayLen => {
                    let target_val = self.stack.pop();
                    let target_addr = target_val.raw() as u64;
                    let len = match Self::find_object_by_addr(&self.heap, target_addr) {
                        Some(crate::memory::Object::Array(gc)) => gc.as_ref().elements.len(),
                        Some(crate::memory::Object::Tuple(gc)) => gc.as_ref().elements.len(),
                        Some(crate::memory::Object::String(gc)) => gc.as_ref().data.len(),
                        Some(crate::memory::Object::Instance(gc)) => gc
                            .as_ref()
                            .slot_len()
                            .unwrap_or_else(|| gc.as_ref().iter_fields().count()),
                        _ => 0,
                    };
                    self.stack.push(Value::from(len as i64));
                }
                Instruction::DictEntries => {
                    // Pop dict → push ObjArray of ObjTuple(2) (key, value).
                    let dict_val = self.stack.pop();
                    let dict_addr = dict_val.raw() as u64;
                    let mut pair_addrs: Vec<Value> = Vec::new();
                    if let Some(crate::memory::Object::Instance(gc)) =
                        Self::find_object_by_addr(&self.heap, dict_addr)
                    {
                        let entries: Vec<(crate::memory::RefString, Member)> =
                            gc.as_ref().iter_fields().collect();
                        for (key, member) in entries {
                            let key_val = Value::from(key.as_ptr() as u64);
                            let val = match member {
                                Member::Value(v) => v,
                                Member::Object(o) => Value::from(o.addr()),
                            };
                            let (tuple_obj, _) = self.heap.alloc(
                                ObjTuple {
                                    elements: vec![key_val, val],
                                },
                                Object::Tuple,
                            );
                            pair_addrs.push(Value::from(tuple_obj.addr()));
                        }
                    }
                    let (array_obj, _) = self.heap.alloc(
                        ObjArray {
                            elements: pair_addrs,
                        },
                        Object::Array,
                    );
                    self.stack.push(Value::from(array_obj.addr()));
                    self.maybe_gc_after_alloc();
                }
                Instruction::JumpIfMatch => {
                    // Tag in operands[31:16]; pool index in operands[15:0]
                    // (`constants[idx]` holds the absolute jump target).
                    let operands = opcode.operand_u32();
                    let expected_tag = operands >> 16;

                    promise!(self.stack.tell() > 0);
                    let scrutinee_addr = self.stack.peek().raw() as u64;

                    let obj_enum = Self::find_enum_exact(&self.heap, scrutinee_addr);

                    if let Some(enum_ref) = obj_enum {
                        let enum_ref = enum_ref.as_ref();
                        if enum_ref.tag == expected_tag {
                            let pool_idx = (operands & 0xFFFF) as usize;
                            promise!(pool_idx < constants.len());
                            let target_offset = opcode.jump_if_match_target(constants);
                            let _ = self.stack.pop();
                            for member in &enum_ref.payload {
                                let value = match member {
                                    Member::Value(v) => *v,
                                    Member::Object(o) => Value::from(o.addr()),
                                };
                                self.stack.push(value);
                            }
                            set_jump_target(&mut ip, target_offset, code);
                        }
                    }
                }
                Instruction::Unpack => {
                    // Pops enum scrutinee; pushes payload in declaration order
                    // (stack/locals overlap — see STORE).
                    let arity = opcode.operand_u32() as usize;

                    promise!(self.stack.tell() > 0);
                    let scrutinee_addr = self.stack.pop().raw() as u64;

                    let obj_enum = Self::find_enum_exact(&self.heap, scrutinee_addr);

                    if let Some(enum_ref) = obj_enum {
                        let enum_ref = enum_ref.as_ref();
                        promise!(arity == enum_ref.payload.len());
                        for i in 0..arity {
                            let member = unsafe { enum_ref.payload.get_unchecked(i) };
                            let value = match member {
                                Member::Value(v) => *v,
                                Member::Object(o) => Value::from(o.addr()),
                            };
                            self.stack.push(value);
                        }
                    }
                }
                Instruction::LoadField => {
                    let field_index = (opcode.operand_u32() & 0xFFFF) as usize;

                    promise!(self.stack.tell() > 0);
                    let scrutinee_addr = self.stack.pop().raw() as u64;

                    match Self::find_object_by_addr(&self.heap, scrutinee_addr) {
                        Some(Object::Enum(enum_ref)) => {
                            let enum_ref = enum_ref.as_ref();
                            promise!(field_index < enum_ref.payload.len());
                            let member = unsafe { enum_ref.payload.get_unchecked(field_index) };
                            let value = match member {
                                Member::Value(v) => *v,
                                Member::Object(o) => Value::from(o.addr()),
                            };
                            self.stack.push(value);
                        }
                        Some(Object::Instance(gc)) => {
                            if let Some(n) = gc.as_ref().slot_len() {
                                promise!(field_index < n);
                                let member = gc
                                    .as_ref()
                                    .slot(field_index)
                                    .unwrap_or(Member::Value(Value::default()));
                                let value = match member {
                                    Member::Value(v) => v,
                                    Member::Object(o) => Value::from(o.addr()),
                                };
                                self.stack.push(value);
                            } else {
                                self.stack.push(Value::default());
                            }
                        }
                        _ => {
                            self.stack.push(Value::default());
                        }
                    }
                }
                Instruction::UnpackAt => {
                    // Unpack enum at `sp + slot_offset` in place (nested record patterns).
                    // Scratch-area codegen may unpack past the current cursor; extend
                    // `tell` so subsequent LOAD/StorePop see the written slots.
                    let operands = opcode.operand_u32();
                    let slot_offset = (operands & 0xFFFF) as usize;
                    let arity = (operands >> 16) as usize;

                    let slot = sp + slot_offset;
                    promise!(slot < self.stack.tell());
                    let scrutinee_addr = self.stack[slot].raw() as u64;

                    let obj_enum = Self::find_enum_exact(&self.heap, scrutinee_addr);

                    if let Some(enum_ref) = obj_enum {
                        let enum_ref = enum_ref.as_ref();
                        promise!(arity == enum_ref.payload.len());
                        for i in 0..arity {
                            let member = unsafe { enum_ref.payload.get_unchecked(i) };
                            let value = match member {
                                Member::Value(v) => *v,
                                Member::Object(o) => Value::from(o.addr()),
                            };
                            self.stack[slot + i] = value;
                        }
                        let end = slot + arity;
                        if self.stack.tell() < end {
                            self.stack.seek(end);
                        }
                    }
                }
                // Deprecated discriminant alias of `STORE` (same handler).
                // Compiler never emits StorePop; kept for archived bytecode.
                Instruction::StorePop => {
                    // Deprecated alias of STORE — same packed multi-slot semantics.
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
                Instruction::MakeCoro => {
                    let (arity, target) = opcode.call_parts();
                    promise!(self.stack.tell() >= arity);
                    let mut values: Vec<Value> = Vec::with_capacity(arity);
                    for _ in 0..arity {
                        values.push(self.stack.pop());
                    }
                    values.reverse();

                    let live_mask = Self::saved_stack_live_mask(&self.heap, &values);
                    let obj_coro = ObjCoroutine {
                        state: CoroState::Suspended,
                        resume_ip: target,
                        saved_stack: values,
                        saved_live_mask: live_mask,
                        saved_frames: vec![(target, 0)],
                        pending_send: Value::from(0_i64),
                        yield_from: None,
                        yield_from_resume_ip: 0,
                        io_wait: None,
                    };
                    let (object, _) = self.heap.alloc(obj_coro, Object::Coroutine);

                    self.stack.push(Value::from(object.addr()));
                    self.maybe_gc_after_alloc();
                }
                Instruction::ResumeCoro => {
                    promise!(self.stack.tell() > 0);
                    let has_send = opcode.operand_u32() & 1 != 0;
                    let handle = self.stack.pop();
                    let send_val = if has_send {
                        promise!(self.stack.tell() > 0);
                        self.stack.pop()
                    } else {
                        Value::from(0_i64)
                    };
                    let addr = handle.raw() as u64;
                    if let Some(Object::Coroutine(gc)) =
                        Self::find_object_by_addr(&self.heap, addr)
                    {
                        if gc.as_ref().state == CoroState::Done {
                            return self.runtime_panic(
                                "resumed after completion",
                                ip.saturating_sub(1),
                            );
                        } else if let Some(sub) = gc.as_ref().yield_from {
                            self.with_coroutine_mut(gc.as_ptr() as u64, |c| {
                                c.pending_send = send_val;
                            });
                            self.resume_coroutine(&mut ip, &mut sp, sub, send_val, code, true);
                        } else {
                            self.resume_coroutine(&mut ip, &mut sp, gc, send_val, code, true);
                        }
                    } else {
                        return self.runtime_panic(
                            "resumed invalid coroutine handle",
                            ip.saturating_sub(1),
                        );
                    }
                }
                Instruction::YieldCoro => {
                    promise!(self.stack.tell() > 0);
                    let yield_val = self.stack.pop();
                    self.yield_coroutine(&mut ip, &mut sp, yield_val);
                }
                Instruction::YieldFromCoro => {
                    promise!(self.stack.tell() > 0);
                    let handle = self.stack.pop();
                    let addr = handle.raw() as u64;
                    if let Some(Object::Coroutine(sub)) =
                        Self::find_object_by_addr(&self.heap, addr)
                    {
                        self.start_yield_from(&mut ip, &mut sp, sub, code);
                    } else {
                        return self.runtime_panic(
                            "yield from invalid coroutine handle",
                            ip.saturating_sub(1),
                        );
                    }
                }
                Instruction::DoneCoro => {
                    promise!(self.stack.tell() > 0);
                    let handle = self.stack.pop();
                    let addr = handle.raw() as u64;
                    let is_done = matches!(
                        Self::find_object_by_addr(&self.heap, addr),
                        Some(Object::Coroutine(gc)) if gc.as_ref().state == CoroState::Done
                    );
                    self.stack.push(Value::from(is_done));
                }
                Instruction::CallIndirect => {
                    // Stack: [value_args..., app_dicts..., target]
                    // operands[15:0] = value_arity; [31:16] = app_dict_arity
                    let packed = opcode.operand_u32();
                    let value_arity = (packed & 0xFFFF) as usize;
                    let app_dict_arity = ((packed >> 16) & 0xFFFF) as usize;
                    promise!(self.stack.tell() >= value_arity + app_dict_arity + 1);
                    let raw = self.stack.pop();

                    // First-class ObjFn: merge new args into holes / captures.
                    let fn_obj = {
                        let addr = raw.raw() as u64;
                        if raw.raw().is_null() {
                            None
                        } else {
                            self.heap.find_object_by_addr(addr).and_then(|o| match o {
                                Object::Fn(gc) => Some(gc),
                                _ => None,
                            })
                        }
                    };

                    if let Some(gc) = fn_obj {
                        // Pop application dictionaries first (unused for ObjFn).
                        for _ in 0..app_dict_arity {
                            let _ = self.stack.pop();
                        }
                        let mut new_args = Vec::with_capacity(value_arity);
                        for _ in 0..value_arity {
                            new_args.push(self.stack.pop());
                        }
                        new_args.reverse();

                        let base = gc.as_ref();
                        let arity = base.arity as usize;
                        let is_rest = base.is_rest;
                        let mut filled_mask = base.filled_mask;
                        let captures = base.captures.clone();
                        let entry = base.entry;

                        // Expand existing filled values into per-slot slots
                        // (decl order), then fill the next unfilled holes
                        // positionally from `new_args`.
                        let mut slot_vals: Vec<Option<Value>> = vec![None; arity];
                        {
                            let mut old_i = 0usize;
                            for slot in 0..arity {
                                if filled_mask & (1u64 << slot) != 0 {
                                    if old_i < base.captured_args.len() {
                                        slot_vals[slot] = Some(base.captured_args[old_i]);
                                        old_i += 1;
                                    }
                                }
                            }
                        }
                        let mut arg_i = 0usize;
                        for slot in 0..arity {
                            if filled_mask & (1u64 << slot) != 0 {
                                continue;
                            }
                            if arg_i >= new_args.len() {
                                break;
                            }
                            slot_vals[slot] = Some(new_args[arg_i]);
                            filled_mask |= 1u64 << slot;
                            arg_i += 1;
                        }

                        let mut captured_args: Vec<Value> = Vec::with_capacity(arity);
                        for slot in 0..arity {
                            if filled_mask & (1u64 << slot) != 0 {
                                if let Some(v) = slot_vals[slot] {
                                    captured_args.push(v);
                                }
                            }
                        }

                        let fixed_filled = filled_mask.count_ones() as usize;
                        let remaining_new = &new_args[arg_i..];

                        if fixed_filled < arity {
                            // Still a partial — push updated ObjFn.
                            let partial = ObjFn {
                                entry,
                                arity: base.arity,
                                is_rest,
                                filled_mask,
                                captured_args,
                                captures,
                            };
                            let (object, _) = self.heap.alloc(partial, Object::Fn);
                            self.stack.push(Value::from(object.addr()));
                            self.maybe_gc_after_alloc();
                            continue;
                        }

                        // Fixed slots complete. Rest extras → MakeArray
                        // (including empty rest when `is_rest` and no extras).
                        let mut call_args = captured_args;
                        if is_rest {
                            let rest_val = if remaining_new.len() == 1 {
                                let v = remaining_new[0];
                                let addr = v.raw() as u64;
                                if !v.raw().is_null()
                                    && matches!(
                                        Self::find_object_by_addr(&self.heap, addr),
                                        Some(Object::Array(_))
                                    )
                                {
                                    v
                                } else {
                                    let arr = crate::memory::ObjArray {
                                        elements: remaining_new.to_vec(),
                                    };
                                    let (object, _) = self.heap.alloc(arr, Object::Array);
                                    Value::from(object.addr())
                                }
                            } else {
                                let arr = crate::memory::ObjArray {
                                    elements: remaining_new.to_vec(),
                                };
                                let (object, _) = self.heap.alloc(arr, Object::Array);
                                Value::from(object.addr())
                            };
                            call_args.push(rest_val);
                        } else if !remaining_new.is_empty() {
                            // Too many args for a fixed fn — drop extras defensively.
                        }

                        // Frame: [captures..., params...]
                        for c in &captures {
                            self.stack.push(*c);
                        }
                        for a in &call_args {
                            self.stack.push(*a);
                        }
                        let frame_arity = captures.len() + call_args.len();
                        let return_ip = ip;
                        let callee_sp = self.stack.tell() - frame_arity;
                        self.frames.get_mut().seek(return_ip);
                        self.frames
                            .setup_current_and_advance(|frame| frame.set(callee_sp));
                        sp = callee_sp;
                        set_jump_target(&mut ip, entry as usize, code);
                        continue;
                    }

                    let (target, captured) = {
                        let addr = raw.raw() as u64;
                        if raw.raw().is_null() {
                            (raw.as_int() as usize, Vec::new())
                        } else if let Some(Object::PolyFn(gc)) = self.heap.find_object_by_addr(addr)
                        {
                            let pfn = gc.as_ref();
                            (pfn.entry as usize, pfn.captured_dicts.clone())
                        } else {
                            (raw.as_int() as usize, Vec::new())
                        }
                    };

                    // Pop application dictionaries (TOS = last in declaration order).
                    let mut app_dicts = Vec::with_capacity(app_dict_arity);
                    for _ in 0..app_dict_arity {
                        app_dicts.push(self.stack.pop());
                    }
                    app_dicts.reverse();

                    let member_value = |m: &crate::memory::Member| -> Value {
                        match m {
                            crate::memory::Member::Value(v) => *v,
                            crate::memory::Member::Object(o) => Value::from(o.addr()),
                        }
                    };

                    let merged_dicts: Vec<Value> = if captured.is_empty() {
                        app_dicts
                    } else {
                        let mut app_i = 0usize;
                        let mut merged = Vec::with_capacity(captured.len());
                        for slot in &captured {
                            match slot {
                                Some(m) => {
                                    merged.push(member_value(m));
                                    if app_i < app_dicts.len() {
                                        app_i += 1;
                                    }
                                }
                                None => {
                                    if app_i < app_dicts.len() {
                                        merged.push(app_dicts[app_i]);
                                        app_i += 1;
                                    } else {
                                        merged.push(Value::default());
                                    }
                                }
                            }
                        }
                        merged
                    };

                    let dict_arity = merged_dicts.len();
                    for dict in merged_dicts {
                        self.stack.push(dict);
                    }

                    let arity = value_arity + dict_arity;

                    let return_ip = ip;
                    let callee_sp = self.stack.tell() - arity;
                    self.frames.get_mut().seek(return_ip);
                    self.frames
                        .setup_current_and_advance(|frame| frame.set(callee_sp));
                    sp = callee_sp;
                    set_jump_target(&mut ip, target, code);
                }
                Instruction::MakeFn => {
                    // Stack (bottom → TOS):
                    //   [captures..., filled_param_values..., filled_mask, entry]
                    // Operand packing:
                    //   [7:0]=n_captures [15:8]=n_filled [23:16]=arity [24]=is_rest
                    let op = opcode.operand_u32();
                    let n_captures = (op & 0xFF) as usize;
                    let n_filled = ((op >> 8) & 0xFF) as usize;
                    let arity = ((op >> 16) & 0xFF) as u32;
                    let is_rest = (op & (1 << 24)) != 0;

                    let entry = self.stack.pop().as_int() as u32;
                    let filled_mask = self.stack.pop().as_int() as u64;

                    let mut filled_vals = Vec::with_capacity(n_filled);
                    for _ in 0..n_filled {
                        filled_vals.push(self.stack.pop());
                    }
                    filled_vals.reverse();

                    let mut captures = Vec::with_capacity(n_captures);
                    for _ in 0..n_captures {
                        captures.push(self.stack.pop());
                    }
                    captures.reverse();

                    let pfn = ObjFn {
                        entry,
                        arity,
                        is_rest,
                        filled_mask,
                        captured_args: filled_vals,
                        captures,
                    };
                    let (object, _) = self.heap.alloc(pfn, Object::Fn);
                    self.stack.push(Value::from(object.addr()));
                    self.maybe_gc_after_alloc();
                }
                Instruction::LoadStatic => {
                    let slot = opcode.operand_u32() as usize;
                    promise!(slot < self.statics.len());
                    let val = self.statics[slot];
                    self.stack.push(val);
                }
                Instruction::StoreStatic => {
                    let slot = opcode.operand_u32() as usize;
                    promise!(slot < self.statics.len());
                    let val = self.stack.pop();
                    self.statics[slot] = val;
                }
                Instruction::BoxValue => {
                    let tag = (opcode.operand_u32() & 0xFFFF) as u16;
                    let v = self.stack.pop();
                    let addr = v.raw() as u64;
                    let payload = if addr == 0 {
                        Member::Value(v)
                    } else if let Some(obj) = Self::find_object_by_addr(&self.heap, addr) {
                        Member::Object(obj)
                    } else {
                        Member::Value(v)
                    };
                    let boxed = ObjBoxed { tag, payload };
                    let (object, _) = self.heap.alloc(boxed, Object::Boxed);
                    self.maybe_gc_after_alloc();
                    self.stack.push(Value::from(object.addr()));
                }
                Instruction::UnboxValue => {
                    let expected_tag = (opcode.operand_u32() & 0xFFFF) as u16;
                    let v = self.stack.pop();
                    let addr = v.raw() as u64;
                    let result = if let Some(Object::Boxed(gc)) =
                        Self::find_object_by_addr(&self.heap, addr)
                    {
                        let b = gc.as_ref();
                        if b.tag == expected_tag {
                            match &b.payload {
                                Member::Value(inner) => *inner,
                                Member::Object(o) => Value::from(o.addr()),
                            }
                        } else {
                            Value::default()
                        }
                    } else {
                        // Already unboxed (e.g. raw enum passed to a Show
                        // thunk that still emits UnboxValue). Pass through.
                        v
                    };
                    self.stack.push(result);
                }
                Instruction::MakePolyFn => {
                    let entry = opcode.operand_u32();
                    let pfn = ObjPolyFn {
                        entry,
                        type_arity: 0,
                        captured_dicts: Vec::new(),
                    };
                    let (object, _) = self.heap.alloc(pfn, Object::PolyFn);
                    self.stack.push(Value::from(object.addr()));
                    self.maybe_gc_after_alloc();
                }
                Instruction::MakePolyFnCapture => {
                    let count = (opcode.operand_u32() & 0xFF) as usize;
                    let entry = self.stack.pop().as_int() as u32;
                    let mut captured_dicts = vec![None; count];
                    for slot in (0..count).rev() {
                        let value = self.stack.pop();
                        let addr = value.raw() as u64;
                        captured_dicts[slot] = if addr == 0 {
                            // Unresolved evidence — filled at CallIndirect.
                            None
                        } else if let Some(obj) = Self::find_object_by_addr(&self.heap, addr) {
                            Some(Member::Object(obj))
                        } else {
                            Some(Member::Value(value))
                        };
                    }
                    let pfn = ObjPolyFn {
                        entry,
                        type_arity: 0,
                        captured_dicts,
                    };
                    let (object, _) = self.heap.alloc(pfn, Object::PolyFn);
                    self.stack.push(Value::from(object.addr()));
                    self.maybe_gc_after_alloc();
                }
                Instruction::DynAdd
                | Instruction::DynSub
                | Instruction::DynMul
                | Instruction::DynDiv
                | Instruction::DynMod => {
                    /// Classify a value into (ValueTag, payload-Value).
                    /// Uses `Heap::find_object_by_addr` (mapped slot + header kind).
                    fn classify_dyn(v: Value, heap: &Heap) -> (ValueTag, Value) {
                        let addr = v.raw() as u64;
                        if v.raw().is_null() {
                            return (ValueTag::Int, v);
                        }
                        if let Some(obj) = heap.find_object_by_addr(addr) {
                            return match obj {
                                Object::Boxed(gc) => {
                                    let b = gc.as_ref();
                                    let tag = ValueTag::from_u16(b.tag).unwrap_or(ValueTag::Int);
                                    let inner = match &b.payload {
                                        Member::Value(iv) => *iv,
                                        Member::Object(o) => Value::from(o.addr()),
                                    };
                                    (tag, inner)
                                }
                                Object::String(_) => (ValueTag::String, v),
                                _ => (ValueTag::Int, v),
                            };
                        }
                        (ValueTag::Int, v)
                    }

                    let b_val = self.stack.pop();
                    let a_val = self.stack.pop();
                    let (a_tag, a_inner) = classify_dyn(a_val, &self.heap);
                    let (b_tag, b_inner) = classify_dyn(b_val, &self.heap);

                    let bc_instr = opcode.bytecode();
                    let result: Value = match (a_tag, b_tag) {
                        (ValueTag::Float, _) | (_, ValueTag::Float) => {
                            let af = a_inner.as_float();
                            let bf = b_inner.as_float();
                            let r = match bc_instr {
                                Instruction::DynAdd => af + bf,
                                Instruction::DynSub => af - bf,
                                Instruction::DynMul => af * bf,
                                Instruction::DynDiv => af / bf,
                                Instruction::DynMod => af % bf,
                                _ => unreachable!(),
                            };
                            Value::from(r)
                        }
                        (ValueTag::String, ValueTag::String)
                            if matches!(bc_instr, Instruction::DynAdd) =>
                        {
                            let sa = Self::object_string_value(&self.heap, &a_inner);
                            let sb = Self::object_string_value(&self.heap, &b_inner);
                            // Root before any GC (same as FORMAT/STRING).
                            self.push_interned_string(sa + &sb);
                            continue;
                        }
                        _ => {
                            let ai = a_inner.as_int();
                            let bi = b_inner.as_int();
                            let r = match bc_instr {
                                Instruction::DynAdd => ai.wrapping_add(bi),
                                Instruction::DynSub => ai.wrapping_sub(bi),
                                Instruction::DynMul => ai.wrapping_mul(bi),
                                Instruction::DynDiv => {
                                    if bi == 0 {
                                        return self.runtime_panic(
                                            "division by zero",
                                            ip.saturating_sub(1),
                                        );
                                    }
                                    ai / bi
                                }
                                Instruction::DynMod => {
                                    if bi == 0 {
                                        return self.runtime_panic(
                                            "division by zero",
                                            ip.saturating_sub(1),
                                        );
                                    }
                                    ai % bi
                                }
                                _ => unreachable!(),
                            };
                            Value::from(r)
                        }
                    };
                    self.stack.push(result);
                }
                Instruction::DynCmp => {
                    fn classify_int_dyn(v: Value, heap: &Heap) -> i64 {
                        let addr = v.raw() as u64;
                        if v.raw().is_null() {
                            return v.as_int();
                        }
                        if let Some(Object::Boxed(gc)) = heap.find_object_by_addr(addr) {
                            return match &gc.as_ref().payload {
                                Member::Value(iv) => iv.as_int(),
                                Member::Object(_) => 0,
                            };
                        }
                        v.as_int()
                    }
                    let kind = opcode.operand_u32() & 0xFF;
                    let b_val = self.stack.pop();
                    let a_val = self.stack.pop();
                    let ai = classify_int_dyn(a_val, &self.heap);
                    let bi = classify_int_dyn(b_val, &self.heap);
                    let result = match kind {
                        0 => ai < bi,  // Le
                        1 => ai <= bi, // Leq
                        2 => ai > bi,  // Gt
                        3 => ai >= bi, // Geq
                        _ => false,
                    };
                    self.stack.push(Value::from(result));
                }
                Instruction::DynEq | Instruction::DynNe => {
                    fn unbox_dyn(v: Value, heap: &Heap) -> Value {
                        let addr = v.raw() as u64;
                        if v.raw().is_null() {
                            return v;
                        }
                        if let Some(Object::Boxed(gc)) = heap.find_object_by_addr(addr) {
                            return match &gc.as_ref().payload {
                                Member::Value(iv) => *iv,
                                Member::Object(o) => Value::from(o.addr()),
                            };
                        }
                        v
                    }
                    let b_val = unbox_dyn(self.stack.pop(), &self.heap);
                    let a_val = unbox_dyn(self.stack.pop(), &self.heap);
                    let eq = crate::value_eq::values_eq(&self.heap, a_val, b_val);
                    let result = if matches!(opcode.bytecode(), Instruction::DynEq) {
                        eq
                    } else {
                        !eq
                    };
                    self.stack.push(Value::from(result));
                }
                Instruction::DynPrint => {
                    let v = self.stack.pop();
                    let text = Self::stringify_value(&self.heap, v);
                    if let Some(out) = self.output.as_mut() {
                        let _ = write!(out, "{text}");
                    } else {
                        print!("{text}");
                    }
                }
                _ => {
                    return self.runtime_panic("unknown opcode", ip.saturating_sub(1));
                }
            }
        }
        false
    }
}

impl<const S: usize> Drop for Machine<S> {
    fn drop(&mut self) {
        self.run_remaining_finalizers();
    }
}


#[cfg(test)]
#[path = "vm.tests.rs"]
mod tests;
