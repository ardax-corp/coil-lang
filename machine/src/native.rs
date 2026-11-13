//! Native code entry points.
//!
//! A backend (`coil-native`, or the interpreter on the same ABI) installs
//! each body here, keyed by its entry id, and the driver runs them.
//!
//! A trap panics natively: the first frame raises it and every frame on the
//! way out adds its site to the trace ([`coil_native_unwind`]). See
//! `docs/internals/native.md`.

/// Shared state between the runtime and native code. `#[repr(C)]`: the native
/// code reads and writes these fields at fixed offsets.
#[repr(C)]
#[derive(Debug)]
pub struct NativeCtx {
    /// Non-zero once a helper stopped the call, or it is unwinding
    /// ([`BAIL_PANIC`]).
    pub bail: u64,
    /// Native code traps when its stack pointer drops below this address,
    /// near the real end of the stack it runs on ([`stack_limit_at`]).
    pub stack_limit: u64,
    /// The machine's heap, for helpers that allocate (null: they bail).
    pub heap: *mut crate::memory::Heap,
    /// Objects helpers allocated during this call. No collection runs
    /// while native code holds raw heap words, so the runtime collects after
    /// the call returns; with a collection due, it collects at its next
    /// safepoint.
    pub allocs: u64,
    /// The second result word of a body that returns two
    /// ([`NativeEntry::ret_words`] `2`): the entry stores it here.
    pub ret_hi: u64,
    /// The program's string table and the machine's cache of its
    /// interned objects (`STRING`), `strings_len` entries each.
    pub strings: *const String,
    pub string_cache: *mut common::Value,
    pub strings_len: u64,
    /// Non-zero when the program has `drop` finalizers: an early free may
    /// then run one, so `FreeEarly` bails and the runtime runs it.
    pub has_finalizers: u64,
    /// The machine's statics (`LoadStatic` / `StoreStatic`), `statics_len`
    /// words.
    pub statics: *mut common::Value,
    pub statics_len: u64,
    /// The [`TRAP_OVERFLOW`]-style code of the trap a helper hit (`0`: an
    /// operation native code does not model).
    pub trap: u64,
    /// While native code unwinds a panic: the site of each native frame,
    /// innermost first (indexes into [`NativeCode::sites`]).
    pub trace: *mut Vec<u32>,
    /// The message of a [`TRAP_MESSAGE`] panic.
    pub message: *mut String,
    /// The machine running the call, for [`NativeHost`] (null: helpers
    /// that need it bail).
    pub machine: *mut std::ffi::c_void,
    pub host: NativeHost,
    /// runtime calls the call made: the runtime then checks for a collection.
    pub host_calls: u64,
    /// The native frames' heap words while a runtime call runs (a collection
    /// may run then): a chain of `[next, n, word × n]` records on the
    /// native stack, newest first, `0`-terminated. The runtime pins them.
    pub roots: u64,
    /// Non-zero when a collection is due and the call can no longer bail
    /// for the runtime to run it: native code collects at its next safepoint
    /// ([`coil_native_collect`]).
    pub gc_due: u64,
    /// Bumped by every point where a collection may have run (a safepoint
    /// collection, a runtime call): native code then reloads its rooted words.
    pub gc_epoch: u64,
    /// Cleanup pads of the frames a panic unwound, innermost
    /// first ([`coil_native_defer`]): the runtime runs them once it reported
    /// the panic, as it runs its own frames' pads.
    pub pads: *mut Vec<NativePad>,
    /// While a native frame makes a call that may run interpreted code (which may
    /// panic and print a backtrace): a chain of `[next, site]` records on
    /// the native stack, innermost first, `0`-terminated, naming each such
    /// frame's call site ([`NativeCode::sites`]).
    pub calls: u64,
    /// The heap's counters and nursery cursors, which inline allocation
    /// reads and bumps ([`crate::memory::inline_object`]); null without a
    /// heap.
    pub alloc_counts: *mut crate::memory::HeapCounts,
    pub alloc_bumps: *mut crate::memory::Bump,
    /// While the call is in a helper that may collect: the frame pointer
    /// of the trampoline native code called it through. The collector
    /// walks the native frames from there ([`register_stack_maps`]).
    pub native_fp: u64,
    /// The running entry's [`NativeEntry::data`] (or a pad's
    /// [`NativePad::data`]): an engine with one entry function for every
    /// body reads which body to run here.
    pub entry_data: u64,
    /// The runtime's step budget (`Runtime::set_step_budget`), for
    /// an engine that counts steps: a charge that reaches zero stops the
    /// call with [`TRAP_BUDGET`].
    pub fuel: *mut u64,
    /// The runtime's host natives: what [`coil_native_host`] may call
    /// (null: no runtime, the standard ones).
    pub natives: *const crate::ffi::Natives,
    /// The runtime's coverage counters ([`coil_native_probe`]).
    pub probes: *mut Vec<u32>,
    /// Non-zero when the call is to park instead of unwinding: see
    /// [`ParkedCall`].
    pub park: u64,
    /// The word a parking call yields.
    pub park_word: u64,
    /// Why the call parked (`PARK_*`), once it did.
    pub park_kind: u64,
    /// What a parked call left, for [`ParkedCall`] (null: it did not park).
    pub parked: *mut std::ffi::c_void,
    pub park_scan: Option<RootScanFn>,
    pub park_free: Option<unsafe fn(*mut std::ffi::c_void)>,
    /// A parked call this one goes on with (null: a new call), and how.
    pub resume: *mut std::ffi::c_void,
    pub resume_word: u64,
    pub resume_how: u64,
    /// The program's compiled bodies, for [`coil_native_call_value`] to
    /// enter a plain function value's body on this context (null: through
    /// the runtime).
    pub code: *const NativeCode,
}

/// Byte offset of [`NativeCtx::native_fp`].
pub const NATIVE_CTX_NATIVE_FP: i32 = std::mem::offset_of!(NativeCtx, native_fp) as i32;

/// Native code's stack maps (Cranelift user stack maps): the code ranges
/// of the compiled bodies, and per call that holds heap words across it,
/// its return address and the offsets from the stack pointer at that call
/// of the slots holding them. A program's code leaves with
/// [`unregister_stack_maps`].
#[derive(Default)]
struct StackMaps {
    /// Start → end of each compiled body's code.
    code: std::collections::BTreeMap<u64, u64>,
    maps: std::collections::HashMap<u64, Box<[u32]>>,
}

static STACK_MAPS: std::sync::LazyLock<std::sync::RwLock<StackMaps>> = std::sync::LazyLock::new(Default::default);

impl StackMaps {
    fn in_code(&self, ra: u64) -> bool {
        self.code.range(..=ra).next_back().is_some_and(|(_, &end)| ra < end)
    }
}

/// Add a compiled body's code (`size` bytes at `start`) and its stack maps:
/// `(return address offset, slot offsets)`. A frame walk goes on through
/// a body's frame whose call holds no heap word, and stops at the first
/// return address outside every body (the entry wrapper or a helper).
pub fn register_stack_maps(start: u64, size: u32, maps: impl IntoIterator<Item = (u32, Vec<u32>)>) {
    let mut table = STACK_MAPS.write().unwrap_or_else(std::sync::PoisonError::into_inner);
    table.code.insert(start, start + u64::from(size));
    for (ra, offsets) in maps {
        table.maps.insert(start + u64::from(ra), offsets.into_boxed_slice());
    }
}

/// Forget `bodies` (`(start, size)`, as registered), whose code is about to go.
/// Its addresses will be some other program's.
pub fn unregister_stack_maps(bodies: &[(u64, u32)]) {
    let gone: std::collections::BTreeMap<u64, u64> = bodies.iter().map(|&(start, size)| (start, start + u64::from(size))).collect();
    let mut table = STACK_MAPS.write().unwrap_or_else(std::sync::PoisonError::into_inner);
    table.code.retain(|start, _| !gone.contains_key(start));
    // A call that does not return can end its body: `ra` is then the end.
    table.maps.retain(|ra, _| !gone.range(..ra).next_back().is_some_and(|(_, end)| ra <= end));
}

/// One AOT stack map record, as `coil_native::aot` writes it: a body's
/// address and code size and the count of its maps, then each map as its
/// return address's offset, its slot count and the slot offsets (`u32`
/// each), the record padded to 8 bytes.
#[repr(C)]
pub struct AotStackMap {
    pub func: u64,
    pub size: u32,
    pub count: u32,
}

/// Register the stack maps an AOT object defines: `n` [`AotStackMap`]
/// records.
///
/// # Safety
/// `maps` points at `n` records as `coil_native::aot` wrote them.
pub unsafe fn install_aot_stack_maps(maps: *const u8, n: usize) {
    let mut at = maps;
    for _ in 0..n {
        // SAFETY: the caller's.
        unsafe {
            let rec = &*at.cast::<AotStackMap>();
            let mut w = at.add(std::mem::size_of::<AotStackMap>()).cast::<u32>();
            let mut out = Vec::with_capacity(rec.count as usize);
            for _ in 0..rec.count {
                let (ra, k) = (*w, *w.add(1) as usize);
                out.push((ra, std::slice::from_raw_parts(w.add(2), k).to_vec()));
                w = w.add(2 + k);
            }
            register_stack_maps(rec.func, rec.size, out);
            let used = w as usize - at as usize;
            at = at.add(used.next_multiple_of(8));
        }
    }
}

/// The host-native groups an AOT executable links ([`install_aot_host_groups`]).
static AOT_HOST_GROUPS: std::sync::OnceLock<Vec<(crate::host_natives::HostPush, u32, u32)>> = std::sync::OnceLock::new();

/// Install the host-native groups an AOT executable's object lists
/// (`coil_aot_host_groups`): its program calls no native outside them.
///
/// # Safety
/// `groups` points at `n` records whose `push` is a `coil_host_group_K` symbol.
pub unsafe fn install_aot_host_groups(groups: *const crate::host_natives::AotHostGroup, n: usize) {
    // SAFETY: the caller's.
    let groups = unsafe { std::slice::from_raw_parts(groups, n) };
    let groups = groups.iter().map(|g| (unsafe { *g.push.cast::<crate::host_natives::HostPush>() }, g.first, g.count)).collect();
    let _ = AOT_HOST_GROUPS.set(groups);
}

/// The host table of an AOT executable: the natives of the groups its
/// object links ([`install_aot_host_groups`]), absent slots elsewhere.
pub fn aot_host_natives() -> Vec<std::sync::Arc<dyn crate::ffi::NativeFn>> {
    crate::host_natives::host_natives_of_groups(AOT_HOST_GROUPS.get().map_or(&[], Vec::as_slice))
}

/// A `defer` cleanup pad: called with its frame's homes.
pub type NativePadFn = unsafe extern "C" fn(ctx: *mut NativeCtx, homes: *const u64) -> u64;

/// Walks an engine's own frames for [`NativeCtx::for_each_root`]: `data`
/// is the record's, and each word the frames hold goes to `visit`, with
/// whether it is ambiguous.
pub type RootScanFn = unsafe fn(data: *mut std::ffi::c_void, visit: &mut dyn FnMut(*mut u64, bool));

/// Walks an engine's own frames for [`NativeCtx::call_sites`]: `data` is
/// the record's, and each frame's call site goes to `out`, innermost first.
pub type SiteScanFn = unsafe fn(data: *mut std::ffi::c_void, out: &mut Vec<u32>);

/// The pad of one native frame a panic unwound, with a copy of its homes
/// laid out as a masked root record (`[next, n | ROOTS_MASKED, masks...,
/// words...]`, see [`NativeCtx::roots`] and [`mask_words`]): a collection
/// while pads run keeps (and may move) the heap words among them.
pub struct NativePad {
    pub func: NativePadFn,
    pub homes: Vec<u64>,
    /// Copied to [`NativeCtx::entry_data`] for the pad.
    pub data: usize,
}

/// Set in a root record's count: the next [`mask_words`] words are a mask
/// of which words are heap words (the rest are scalars to skip), bit `i`
/// of word `i / 64` for word `i`.
pub const ROOTS_MASKED: u64 = 1 << 63;

/// How many mask words a masked root record of `n` words carries: one per
/// 64 words, and at least one.
pub const fn mask_words(n: usize) -> usize {
    if n == 0 { 1 } else { n.div_ceil(64) }
}

/// The masks of a masked root record of `n` words whose heap words are
/// `heap` ([`ROOTS_MASKED`]).
pub fn root_masks(n: usize, heap: impl IntoIterator<Item = usize>) -> Vec<u64> {
    let mut masks = vec![0u64; mask_words(n)];
    for h in heap {
        masks[h / 64] |= 1 << (h % 64);
    }
    masks
}

/// Set in a root record's count: its words are ambiguous (an opaque word
/// of a generic body may be an immediate), so the collector pins what they
/// hit instead of moving it, and never rewrites them.
pub const ROOTS_AMBIGUOUS: u64 = 1 << 62;
/// A [`NativeCtx::roots`] record of an engine that keeps its frames itself:
/// `[next, ROOTS_SCAN, RootScanFn, data]`. Native code copies its words
/// into a record around each call; such an engine has them in place, so
/// the collector asks for them only when it runs.
pub const ROOTS_SCAN: u64 = 1 << 61;
/// In a [`NativeCtx::calls`] record's site word: `[next, SITES_SCAN,
/// SiteScanFn, data]`, as [`ROOTS_SCAN`] for call sites.
pub const SITES_SCAN: u64 = u64::MAX;

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

pub use crate::memory::{InlineObject, InlineShape, ProductSlots, inline_object, product_slots, layout_fingerprint};

/// Byte offsets of [`NativeCtx::alloc_counts`] and [`NativeCtx::alloc_bumps`].
pub const NATIVE_CTX_ALLOC_COUNTS: i32 = std::mem::offset_of!(NativeCtx, alloc_counts) as i32;
pub const NATIVE_CTX_ALLOC_BUMPS: i32 = std::mem::offset_of!(NativeCtx, alloc_bumps) as i32;

/// Byte offsets in [`crate::memory::HeapCounts`] and [`crate::memory::Bump`]
/// inline allocation reads and writes.
pub const COUNTS_INLINE_GATE: i32 = std::mem::offset_of!(crate::memory::HeapCounts, inline_gate) as i32;
pub const COUNTS_FIN_WORDS: i32 = std::mem::offset_of!(crate::memory::HeapCounts, fin_words) as i32;
pub const COUNTS_FIN_LEN: i32 = std::mem::offset_of!(crate::memory::HeapCounts, fin_len) as i32;
pub const COUNTS_ALLOC_BYTES: i32 = std::mem::offset_of!(crate::memory::HeapCounts, alloc_bytes) as i32;
pub const COUNTS_YOUNG_BYTES: i32 = std::mem::offset_of!(crate::memory::HeapCounts, young_bytes) as i32;
pub const COUNTS_LIVE_COUNT: i32 = std::mem::offset_of!(crate::memory::HeapCounts, live_count) as i32;
pub const BUMP_NEXT: i32 = std::mem::offset_of!(crate::memory::Bump, next) as i32;
pub const BUMP_END: i32 = std::mem::offset_of!(crate::memory::Bump, end) as i32;
pub const BUMP_BYTES: i32 = std::mem::size_of::<crate::memory::Bump>() as i32;

impl NativeCtx {
    /// Every heap word the call's native frames hold while it is in a runtime
    /// call: the root chain.
    /// The words are native code's to reload: a moving collection may
    /// rewrite them.
    /// The sites of [`Self::calls`], innermost first.
    pub(crate) fn call_sites(&self) -> Vec<u32> {
        let mut out = Vec::new();
        let mut at = self.calls as *const u64;
        while !at.is_null() {
            // SAFETY: native code links only live records of its frames.
            unsafe {
                let site = *at.add(1);
                if site == SITES_SCAN {
                    let scan = std::mem::transmute::<usize, SiteScanFn>(*at.add(2) as usize);
                    scan(*at.add(3) as *mut std::ffi::c_void, &mut out);
                } else {
                    out.push(site as u32);
                }
                at = *at as *const u64;
            }
        }
        out
    }

    pub(crate) fn for_each_root(&self, mut visit: impl FnMut(*mut u64, bool)) {
        self.for_each_frame_root(&mut visit);
        let mut at = self.roots as *mut u64;
        while !at.is_null() {
            // SAFETY: native code links only live records of its frames.
            unsafe {
                let n = *at.add(1);
                let ambiguous = n & ROOTS_AMBIGUOUS != 0;
                let count = n & !(ROOTS_MASKED | ROOTS_AMBIGUOUS);
                if n & ROOTS_SCAN != 0 {
                    let scan = std::mem::transmute::<usize, RootScanFn>(*at.add(2) as usize);
                    scan(*at.add(3) as *mut std::ffi::c_void, &mut visit);
                } else if n & ROOTS_MASKED != 0 {
                    let words = mask_words(count as usize);
                    for i in 0..count as usize {
                        if *at.add(2 + i / 64) & (1 << (i % 64)) != 0 {
                            visit(at.add(2 + words + i), ambiguous);
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
    }
}

impl NativeCtx {
    /// The heap words the native frames of this call hold while it is in
    /// a helper that may collect: from the trampoline's frame
    /// ([`Self::native_fp`]) up the frame-pointer chain, each frame's
    /// stack map at the call it is in. A collection may rewrite the slots
    /// (native code reloads them after the call).
    fn for_each_frame_root(&self, visit: &mut impl FnMut(*mut u64, bool)) {
        let mut fp = self.native_fp;
        if fp == 0 {
            return;
        }
        let maps = STACK_MAPS.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        while fp != 0 {
            // SAFETY: native code keeps frame pointers: `[fp]` is the
            // caller's frame pointer and `[fp + 8]` the return address into
            // it. Slots are by their distance below that frame pointer.
            let (next, ra) = unsafe { (*(fp as *const u64), *(fp as *const u64).add(1)) };
            if !maps.in_code(ra) {
                break;
            }
            for &off in maps.maps.get(&ra).into_iter().flatten() {
                visit((next - u64::from(off)) as *mut u64, false);
            }
            fp = next;
        }
    }
}

/// What native code asks of the machine that runs it, type-erased over its
/// stack size. Each takes [`NativeCtx::machine`].
#[derive(Clone, Copy, Debug)]
pub struct NativeHost {
    /// Write `text` as `PRINT` does.
    pub print: unsafe fn(*mut std::ffi::c_void, &str),
    /// The running task and the live ones, for a debugger: see
    /// [`debug_tasks`].
    pub tasks: unsafe fn(*mut std::ffi::c_void) -> DebugTasks,
    /// What a call of function word `callee` on the words comes to, the last
    /// `usize` of them dictionaries: see [`apply_value`].
    pub apply: unsafe fn(*mut std::ffi::c_void, u64, &[common::Value], usize) -> Applied,
    /// Call the function value (or code pointer) word on `args`, the last
    /// `u32` of them instance dictionaries, as `CallIndirect` does; same
    /// result and panic rules as `call`.
    pub call_value: unsafe fn(*mut std::ffi::c_void, *mut NativeCtx, u64, &[common::Value], u32, &mut bool) -> u64,
    /// Before a call of the body at `pc` on `word`: `1` when `pc` is no
    /// `drop`; else, claiming the finalizer, `2` when this
    /// call claimed it and `0` when it ran already (skip the call).
    pub claim: unsafe fn(*mut std::ffi::c_void, u32, u64) -> u64,
    /// Resume the generator word (with the sent word when the `bool` says
    /// so), as `ResumeCoro` does: its body runs on the runtime until it yields
    /// or returns, and that word comes back; same panic rules as `call`.
    pub resume: unsafe fn(*mut std::ffi::c_void, *mut NativeCtx, u64, Option<u64>, &mut bool) -> u64,
    /// Collect, with the context's roots pinned.
    pub collect: unsafe fn(*mut std::ffi::c_void, *mut NativeCtx),
    /// `gc_collect`: a full collection, with the context's roots pinned;
    /// returns the bytes freed and sets the `bool` when a `drop` panicked.
    pub gc: unsafe fn(*mut std::ffi::c_void, *mut NativeCtx, &mut bool) -> u64,
    /// `FreeEarly` now, running the `drop`s it owes (with the context's
    /// roots pinned); `false` when one panicked (the runtime reported it).
    pub free: unsafe fn(*mut std::ffi::c_void, *mut NativeCtx, u64, bool) -> bool,
    /// Run the FFI op (`FfiLoad` / `DeclareFFI` / `FfiInvoke`, by opcode
    /// byte) with its operand on `args`, on the runtime.
    pub ffi: MachineHostFn,
    /// Run the ordinary host native `id` on `args` in `layout` as the runtime's
    /// `HostInvoke` does, without interpreting a body: a native that would block the
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
    /// The task was cancelled while it waited: the runtime armed the unwind.
    Unwound,
}


/// What calling a function value comes to.
pub enum Applied {
    /// A value at once (a partial application).
    Value(u64),
    /// A call of the body at `pc` on these words, for an engine to run as
    /// one of its own frames.
    Call { pc: usize, args: Vec<common::Value> },
    /// Anything else: [`coil_native_call_value`] does it.
    Other,
}

/// See [`Applied`].
///
/// # Safety
/// `ctx` is a live call's [`NativeCtx`]; `args` points at `n` words.
pub unsafe fn apply_value(ctx: *mut NativeCtx, callee: u64, args: *const u64, n: u64, dicts: u64) -> Applied {
    let c = unsafe { &*ctx };
    if c.machine.is_null() {
        return Applied::Other;
    }
    // SAFETY: as this function's.
    let words: Vec<common::Value> = (0..n as usize).map(|i| common::Value::from(unsafe { *args.add(i) })).collect();
    // SAFETY: `machine` is the live machine `host` was made for.
    unsafe { (c.host.apply)(c.machine, callee, &words, dicts as usize) }
}

/// The running task's id and each live task's `(id, name, state)`.
pub type DebugTasks = (u64, Vec<(u64, String, String)>);

/// The tasks of the run `ctx` belongs to. No scheduler: task `0` and no list.
///
/// # Safety
/// `ctx` is a live call's [`NativeCtx`].
pub unsafe fn debug_tasks(ctx: *mut NativeCtx) -> DebugTasks {
    let c = unsafe { &*ctx };
    if c.machine.is_null() {
        return (0, Vec::new());
    }
    // SAFETY: `machine` is the live machine `host` was made for.
    unsafe { (c.host.tasks)(c.machine) }
}

/// The message of the trap `code` stopped the call with.
///
/// # Safety
/// `ctx` is a live call's [`NativeCtx`].
pub unsafe fn trap_text(ctx: *mut NativeCtx, code: u64) -> String {
    let c = unsafe { &*ctx };
    let code = if code == TRAP_HELPER { c.trap } else { code };
    // SAFETY: `call` set `message` for the call.
    let message = if c.message.is_null() { String::new() } else { unsafe { (*c.message).clone() } };
    NativePanic { code, message, trace: Vec::new(), pads: Vec::new() }.message().to_string()
}

/// [`NativeHost::host`]: machine, context, native id, layout, arguments,
/// panicked flag.
pub type MachineHostFn = unsafe fn(*mut std::ffi::c_void, *mut NativeCtx, u64, u64, &[common::Value], &mut bool) -> Option<u64>;

impl NativeHost {
    const NONE: Self = Self {
        print: |_, _| {},
        tasks: |_| (0, Vec::new()),
        apply: |_, _, _, _| Applied::Other,
        call_value: |_, _, _, _, _, _| 0,
        resume: |_, _, _, _, _| 0,
        claim: |_, _, _| 1,
        collect: |_, _| {},
        gc: |_, _, _| 0,
        free: |_, _, _, _| true,
        ffi: |_, _, _, _, _, _| None,
        invoke: |_, _, _, _, _| None,
    };
}

/// The machine a native call runs on, as raw pointers: native code and the
/// runtime calls it makes reach the machine while the call runs, so `call`
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
    pub(crate) fuel: *mut u64,
    pub(crate) natives: *const crate::ffi::Natives,
    pub(crate) probes: *mut Vec<u32>,
    /// The compiled bodies ([`NativeCtx::code`]).
    pub(crate) code: *const NativeCode,
}



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
/// A runtime call native code made panicked; the runtime already reported it.
pub const TRAP_PANICKED: u64 = 8;
/// An explicit `panic(msg)`: the message in [`NativeCtx::message`].
pub const TRAP_EXPLICIT: u64 = 9;
/// Not a panic: the call's fiber parked (its task suspended inside it);
/// the task suspends at the `CALL` (`driver.rs`).
pub const TRAP_PARKED: u64 = 10;
/// Not a panic: a generator's close unwinds its body from the yield it is
/// suspended at, collecting its armed `defer`s (`gen_fiber.rs`).
pub const TRAP_CLOSE: u64 = 11;
/// The step budget ran out, or the heap outgrew its cap, which drains it.
pub const TRAP_BUDGET: u64 = 12;
/// No site: a frame that pushes nothing onto the panic's trace.
pub const NO_SITE: u64 = u32::MAX as u64;

/// [`NativeCtx::bail`] of a call that is unwinding (`1`: a helper stopped it).
pub const BAIL_PANIC: u64 = 2;

/// Byte offsets of [`NativeCtx::statics`] and [`NativeCtx::statics_len`].
pub const NATIVE_CTX_STATICS: i32 = std::mem::offset_of!(NativeCtx, statics) as i32;
pub const NATIVE_CTX_STATICS_LEN: i32 = std::mem::offset_of!(NativeCtx, statics_len) as i32;

/// Byte offset of [`NativeCtx::ret_hi`].
pub const NATIVE_CTX_RET_HI: i32 = std::mem::offset_of!(NativeCtx, ret_hi) as i32;

/// Byte offset of [`NativeCtx::bail`].
pub const NATIVE_CTX_BAIL: i32 = 0;
/// Byte offset of [`NativeCtx::stack_limit`].
pub const NATIVE_CTX_STACK_LIMIT: i32 = 8;

impl NativeCtx {
    /// A context for one call from the runtime.
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
            has_finalizers: 0,
            statics: std::ptr::null_mut(),
            statics_len: 0,
            trap: 0,
            trace: std::ptr::null_mut(),
            message: std::ptr::null_mut(),
            machine: std::ptr::null_mut(),
            host: NativeHost::NONE,
            host_calls: 0,
            roots: 0,
            gc_due: 0,
            gc_epoch: 0,
            pads: std::ptr::null_mut(),
            calls: 0,
            alloc_counts: std::ptr::null_mut(),
            alloc_bumps: std::ptr::null_mut(),
            native_fp: 0,
            entry_data: 0,
            fuel: std::ptr::null_mut(),
            natives: std::ptr::null(),
            probes: std::ptr::null_mut(),
            park: 0,
            park_word: 0,
            park_kind: 0,
            parked: std::ptr::null_mut(),
            park_scan: None,
            park_free: None,
            resume: std::ptr::null_mut(),
            resume_word: 0,
            resume_how: 0,
            code: std::ptr::null(),
        }
    }

    /// As [`Self::new`], with `heap` for the helpers that allocate.
    ///
    /// # Safety
    /// `heap` is null or the live heap the call runs on.
    pub(crate) unsafe fn with_heap(stack_limit: u64, heap: *mut crate::memory::Heap) -> Self {
        // SAFETY: the caller's.
        let (alloc_counts, alloc_bumps) =
            if heap.is_null() { (std::ptr::null_mut(), std::ptr::null_mut()) } else { unsafe { (*heap).inline_alloc_ptrs() } };
        Self { heap, alloc_counts, alloc_bumps, ..Self::new(stack_limit) }
    }
}

/// How much stack a call may use when the stack's real end is not known.
pub const NATIVE_STACK_BUDGET: u64 = 256 * 1024;

/// Stack native code leaves unused at the stack's real end.
pub const NATIVE_STACK_GUARD: u64 = 128 * 1024;

/// Entry of a native function: `args` holds `arity` words, the result
/// is one word. Sets `ctx.bail` instead of returning a result when it
/// panics.
pub type NativeEntryFn = unsafe extern "C" fn(ctx: *mut NativeCtx, args: *const u64) -> u64;

/// One compiled function.
#[derive(Clone, Copy, Debug)]
pub struct NativeEntry {
    /// Entry id of the function's entry (its `CALL` target).
    pub pc: u32,
    pub arity: u16,
    /// It may run interpreted code (a runtime call, a function value, a `drop`), so a
    /// task switch could come while its frames are live: with a scheduler
    /// that can switch, the call runs on the task's own fiber stack
    /// (`driver.rs`).
    pub suspends: bool,
    /// The leading parameters the body reads: a `CALL` of at least this
    /// many words (and at most `arity`) enters it, the rest passed as `0`.
    pub reads: u16,
    /// Result words: `1`, or `2` for a two-slot return (`lo`, then `hi`
    /// in [`NativeCtx::ret_hi`]), in that order.
    pub ret_words: u8,
    /// A generator's body: only a resume enters it, on a fiber of its own
    /// (`gen_fiber.rs`), never a `CALL`.
    pub generator: bool,
    /// It has `defer`s (a cleanup pad): a generator's close resumes its
    /// fiber to unwind them (`gen_fiber.rs`).
    pub cleanup: bool,
    pub entry: NativeEntryFn,
    /// Copied to [`NativeCtx::entry_data`] for the call (`0` for machine
    /// code, whose `entry` is the body).
    pub data: usize,
    /// One past the entry id of the body's two-word twin
    /// (`MirFunc::pair_twin`), `0` for none: a `CallIndirect` of a
    /// two-word result enters it ([`coil_native_fn_entry`]).
    pub twin: u32,
}

/// A [`NativeEntry`] as an AOT object's `coil_aot_entries` table lays it
/// out (`coil_native::aot` writes it, [`install_aot`] reads it).
#[repr(C)]
pub struct AotEntry {
    pub pc: u32,
    pub arity: u16,
    pub reads: u16,
    pub ret_words: u8,
    /// `1` when it may run interpreted code ([`NativeEntry::suspends`]).
    pub suspends: u8,
    /// `1` for a generator's body ([`NativeEntry::generator`]).
    pub generator: u8,
    /// `1` when it has `defer`s ([`NativeEntry::cleanup`]).
    pub cleanup: u8,
    /// [`NativeEntry::twin`].
    pub twin: u32,
    pub entry: NativeEntryFn,
}

/// Install the code an AOT object linked into this executable defines,
/// for the next run: `n` entries and `n_sites` sites, each five words
/// (`body_id`, then the location's file, start and end byte and inline
/// scope).
/// `complete`: the object compiled every body of the program. `layout`:
/// the [`layout_fingerprint`] of the compiler that made the object; an
/// object made against other object layouts is refused (`Err`), since its
/// inline allocation would write them wrong.
///
/// # Safety
///
/// The tables are the linked object's, as `coil_native::aot` wrote them.
pub unsafe fn install_aot(entries: *const AotEntry, n: usize, sites: *const u32, n_sites: usize, complete: bool, layout: u64) -> Result<(), String> {
    if layout != layout_fingerprint() {
        return Err("this executable's native code was compiled against another build of the runtime (object layouts differ); package it again".to_string());
    }
    // SAFETY: the object defines `n` entries and `5 * n_sites` words.
    let entries = unsafe { std::slice::from_raw_parts(entries, n) };
    let words = unsafe { std::slice::from_raw_parts(sites, n_sites * 5) };
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
            data: 0,
            twin: e.twin,
        })
        .collect();
    let sites = words
        .as_chunks::<5>()
        .0
        .iter()
        .map(|w| NativeSite { body_id: w[0], loc: common::DebugLoc { file: w[1], start_byte: w[2], end_byte: w[3], scope: w[4] } })
        .collect();
    install_for_next_run(entries, sites, complete);
    Ok(())
}

/// An op native code may panic at: the function it is in (by entry id)
/// and the op's source location. A native panic's trace names one per
/// native frame, which the runtime prints as it prints its own frames.
#[derive(Clone, Copy, Debug)]
pub struct NativeSite {
    pub body_id: u32,
    pub loc: common::DebugLoc,
}

/// The compiled functions of one program, indexed by entry id. Shared
/// (`Arc`) by every machine that runs the program.
#[derive(Default)]
pub struct NativeCode {
    /// `by_pc[pc]` is `index + 1` into `entries`, or `0`.
    by_pc: Vec<u32>,
    entries: Vec<NativeEntry>,
    sites: Vec<NativeSite>,
    /// Every body of the program compiled: the driver can run all of it.
    complete: bool,
    /// What [`NativeEntry::data`] points into, kept for as long as the code.
    keep: Option<std::sync::Arc<dyn std::any::Any + Send + Sync>>,
}

impl NativeCode {
    /// `sites` are the panic sites the code names.
    pub fn new(entries: Vec<NativeEntry>, sites: Vec<NativeSite>) -> Self {
        let code_len = entries.iter().map(|e| e.pc as usize + 1).max().unwrap_or(0);
        let mut by_pc = vec![0u32; code_len];
        let mut kept = Vec::with_capacity(entries.len());
        for e in entries {
            if let Some(slot) = by_pc.get_mut(e.pc as usize) {
                kept.push(e);
                *slot = kept.len() as u32;
            }
        }
        Self { by_pc, entries: kept, sites, complete: true, keep: None }
    }

    /// The code, keeping `owner` alive with it: an engine's entries point
    /// into it ([`NativeEntry::data`]).
    pub fn with_keep(mut self, owner: std::sync::Arc<dyn std::any::Any + Send + Sync>) -> Self {
        self.keep = Some(owner);
        self
    }

    /// The code, marked as covering every body of the program or not
    /// (some refused): only complete code runs on the driver.
    pub fn with_complete(mut self, complete: bool) -> Self {
        self.complete = complete;
        self
    }

    /// Every body of the program compiled.
    pub(crate) fn is_complete(&self) -> bool {
        self.complete
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

    /// [`Self::lookup`] without copying the entry.
    #[inline]
    pub(crate) fn entry_ref(&self, pc: usize) -> Option<&NativeEntry> {
        let i = *self.by_pc.get(pc)?;
        self.entries.get((i as usize).checked_sub(1)?)
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
    /// Calls that ran natively (`COIL_NATIVE_STATS=1`).
    pub(crate) hits: u64,
}

impl NativeTable {
    pub(crate) fn new(code: std::sync::Arc<NativeCode>) -> Self {
        Self {
            code,
            hits: 0,
        }
    }

    #[inline]
    pub(crate) fn lookup(&self, pc: usize) -> Option<NativeEntry> {
        self.code.lookup(pc)
    }
}

/// What a native call gave back: its result words and whether it
/// allocated (the runtime then checks for a collection).
pub(crate) struct NativeResult {
    pub(crate) lo: u64,
    pub(crate) hi: Option<u64>,
    pub(crate) allocated: bool,
}

/// A panic native code raised: the [`TRAP_OVERFLOW`]
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

    /// The runtime's message for the same trap.
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
    Panicked(NativePanic),
    /// It parked ([`parks`]): the word it yielded, if any.
    Parked(ParkedCall, u64),
}

/// The [`NativeCtx::stack_limit`] of a call at `sp`: near the real end of
/// the stack it is on, when that is known. Not the lower of that and the
/// budget: near the end, `sp` less the budget is past it, and each nested
/// entry would then get a limit it can never reach.
fn stack_limit_at(sp: u64) -> u64 {
    if let Some(limit) = crate::stacks::fiber_stack_limit() {
        limit as u64
    } else if let Some(left) = stacker::remaining_stack() {
        sp.saturating_sub((left as u64).saturating_sub(NATIVE_STACK_GUARD))
    } else {
        sp.saturating_sub(NATIVE_STACK_BUDGET)
    }
}

/// Whether generators and tasks park their calls on the heap instead of
/// running on fibers: where there are none (wasm), and for a run of the
/// interpreter under `COIL_NO_FIBERS`, which tests that path natively.
pub fn parks() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    cfg!(target_family = "wasm") || *ON.get_or_init(|| std::env::var_os("COIL_NO_FIBERS").is_some())
}

/// [`NativeCtx::park`]: the call yielded.
pub const PARK_YIELD: u64 = 1;
/// The call's task waits, at this op.
pub const PARK_WAIT: u64 = 2;
/// The call's task waits, in a generator this op resumed: the op runs again
/// on the wake, and the wake goes on down to the wait.
pub const PARK_BELOW: u64 = 3;

/// [`ParkedCall::resume`]: the op it parked at gets this word.
pub const RESUME_VALUE: u64 = 1;
/// Run the op it parked at again.
pub const RESUME_RETRY: u64 = 2;
/// Unwind from it: a generator closed.
pub const RESUME_CLOSE: u64 = 3;
/// Unwind from it: a task cancelled.
pub const RESUME_CANCEL: u64 = 4;

/// A call that stopped at a yield or a wait with its frames on the heap, for
/// an engine that keeps them there (the interpreter): what a fiber is where
/// the stack can be switched.
pub(crate) struct ParkedCall {
    entry: NativeEntry,
    /// Why it parked (`PARK_*`).
    pub(crate) kind: u64,
    state: *mut std::ffi::c_void,
    scan: RootScanFn,
    free: unsafe fn(*mut std::ffi::c_void),
}

impl GenFiber {
    /// The side of a generator whose call parked.
    pub(crate) fn parked(call: ParkedCall) -> Self {
        let cleanup = call.cleanup();
        Self {
            #[cfg(not(target_family = "wasm"))]
            coro: None,
            stack_limit: 0,
            yielder: std::ptr::null(),
            parked: std::ptr::null_mut(),
            call: Some(call),
            cleanup,
        }
    }
}

impl ParkedCall {
    /// Whether its body has `defer`s, which a close must run.
    pub(crate) fn cleanup(&self) -> bool {
        self.entry.cleanup
    }

    /// Its heap words, for a root that pins them.
    pub(crate) fn for_each_word(&self, mut visit: impl FnMut(u64)) {
        // SAFETY: the engine's own state, read only.
        unsafe { (self.scan)(self.state, &mut |w, _| visit(*w)) };
    }

    /// Its heap words, as [`NativeCtx::for_each_root`] walks a live call's.
    pub(crate) fn for_each_root(&mut self, mut visit: impl FnMut(*mut u64, bool)) {
        // SAFETY: the engine's own state, which nothing else runs.
        unsafe { (self.scan)(self.state, &mut visit) };
    }

    /// Go on, in the way `how` says (`RESUME_*`) with `word`.
    pub(crate) fn resume(mut self, word: u64, how: u64, env: &NativeEnv) -> NativeOutcome {
        let entry = self.entry;
        let state = std::mem::replace(&mut self.state, std::ptr::null_mut());
        run_call(&entry, &[], Some((state, word, how)), env)
    }
}

impl Drop for ParkedCall {
    fn drop(&mut self) {
        if !self.state.is_null() {
            // SAFETY: it never runs again.
            unsafe { (self.free)(self.state) };
        }
    }
}

/// Unwind from a parked generator's yield: it closed.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`].
pub unsafe fn raise_close(ctx: *mut NativeCtx) {
    let c = unsafe { &mut *ctx };
    c.trap = TRAP_CLOSE;
    c.bail = BAIL_PANIC;
}

/// Unwind from a parked task's wait: it was cancelled.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`].
pub unsafe fn raise_cancel(ctx: *mut NativeCtx) {
    let c = unsafe { &mut *ctx };
    c.trap = TRAP_PANICKED;
    c.bail = 1;
}

/// Run `entry` with `args`.
pub(crate) fn call(entry: &NativeEntry, args: &[u64], env: &NativeEnv) -> NativeOutcome {
    run_call(entry, args, None, env)
}

/// Run `entry` with `args`, or go on with parked state `(state, word, how)`.
#[inline(never)]
fn run_call(entry: &NativeEntry, args: &[u64], resume: Option<(*mut std::ffi::c_void, u64, u64)>, env: &NativeEnv) -> NativeOutcome {
    let marker = 0u8;
    let sp = std::ptr::addr_of!(marker) as u64;
    let mut ctx = unsafe { NativeCtx::with_heap(stack_limit_at(sp), env.heap) };
    ctx.machine = env.machine;
    ctx.host = env.host;
    ctx.fuel = env.fuel;
    ctx.natives = env.natives;
    ctx.probes = env.probes;
    ctx.strings = env.strings;
    ctx.string_cache = env.string_cache;
    ctx.strings_len = env.strings_len as u64;
    ctx.has_finalizers = u64::from(env.has_finalizers);
    ctx.statics = env.statics;
    ctx.statics_len = env.statics_len as u64;
    ctx.code = env.code;
    let mut trace = Vec::new();
    ctx.trace = &mut trace;
    let mut message = String::new();
    ctx.message = &mut message;
    let mut pads = Vec::new();
    ctx.pads = &mut pads;
    if let Some((state, word, how)) = resume {
        ctx.resume = state;
        ctx.resume_word = word;
        ctx.resume_how = how;
    }
    ctx.entry_data = entry.data as u64;
    // A speculative job's call can be cancelled: its stack limit is
    // poisoned then (#2442). Listed until the call returns.
    let cancel = crate::thread::CallGuard::enter(&raw mut ctx.stack_limit);
    // SAFETY: the backend compiled `entry` for exactly `arity` argument
    // words, which `args` holds; `ctx` outlives the call.
    let out = unsafe { (entry.entry)(&mut ctx, args.as_ptr()) };
    drop(cancel);
    if !ctx.parked.is_null() {
        let (Some(scan), Some(free)) = (ctx.park_scan, ctx.park_free) else {
            unreachable!("a parked call names its engine's hooks");
        };
        let call = ParkedCall { entry: *entry, kind: ctx.park_kind, state: ctx.parked, scan, free };
        return NativeOutcome::Parked(call, ctx.park_word);
    }
    match ctx.bail {
        0 => {
            let hi = (entry.ret_words == 2).then_some(ctx.ret_hi);
            NativeOutcome::Done(NativeResult { lo: out, hi, allocated: ctx.allocs != 0 || ctx.host_calls != 0 })
        }
        // Any flag is a panic: its effects stay.
        _ => NativeOutcome::Panicked(NativePanic { code: ctx.trap, trace, message, pads }),
    }
}

/// Where native code stops a call: called by every frame on the way out,
/// with the frame's `site` ([`NO_SITE`] for none) and the reason `code`.
/// The first frame raises a panic and every frame adds its site to the
/// trace.
///
/// # Safety
/// `ctx` is the call's live [`NativeCtx`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_unwind(ctx: *mut NativeCtx, code: u64, site: u64) {
    let ctx = unsafe { &mut *ctx };
    if ctx.bail != BAIL_PANIC {
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

/// Native code for the next run on this thread
/// (the program's main machine); the machines it spawns share it.
/// `complete`: every body of the program compiled.
pub fn install_for_next_run(entries: Vec<NativeEntry>, sites: Vec<NativeSite>, complete: bool) {
    PENDING.with(|p| *p.borrow_mut() = Some(Pending::Entries(entries, sites, complete)));
}

/// [`install_for_next_run`] with code already built for the program.
pub fn install_shared_for_next_run(code: std::sync::Arc<NativeCode>) {
    PENDING.with(|p| *p.borrow_mut() = Some(Pending::Shared(code)));
}

pub(crate) enum Pending {
    Entries(Vec<NativeEntry>, Vec<NativeSite>, bool),
    Shared(std::sync::Arc<NativeCode>),
}

impl Pending {
    pub(crate) fn into_code(self) -> std::sync::Arc<NativeCode> {
        match self {
            Self::Entries(entries, sites, complete) => std::sync::Arc::new(NativeCode::new(entries, sites).with_complete(complete)),
            Self::Shared(code) => code,
        }
    }
}

/// `COIL_NATIVE_STATS`'s counts after a run with native code installed.
pub(crate) fn print_stats(table: &NativeTable) {
    eprintln!("native: {} calls ran natively", table.hits);
    let vector = VECTOR_OPS.load(std::sync::atomic::Ordering::Relaxed);
    if vector > 0 {
        eprintln!("native: {vector} vector ops ran natively");
    }
}

/// Whether the native code pending for the next run covers every body.
pub(crate) fn pending_is_complete() -> bool {
    PENDING.with(|p| match &*p.borrow() {
        Some(Pending::Entries(entries, _, complete)) => *complete && !entries.is_empty(),
        Some(Pending::Shared(code)) => code.is_complete() && !code.is_empty(),
        None => false,
    })
}

pub(crate) fn take_pending() -> Option<Pending> {
    PENDING.with(|p| p.borrow_mut().take())
}

// ---------------------------------------------------------------------------
// Heap reads for native code (heap tier).
//
// Native code calls these with heap words it was handed. It never
// re-enters the runtime and its helpers never collect (the Cow helpers below
// allocate, but the runtime collects only after the call returns), so every
// address it holds stays a live object at the same place. Each helper does what
// the matching runtime instruction does; where the runtime would panic, it sets the
// bail flag and returns 0.

/// The object at a heap word, as `trusted_object` reads it.
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
        Some(crate::memory::Object::Enum(gc)) if gc.as_ref().is_tuple() => {
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
                if object(addr).is_some_and(|o| matches!(o, crate::memory::Object::Array(_)) || o.as_tuple().is_some()) {
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
        Some(crate::memory::Object::Enum(gc)) if gc.as_ref().is_tuple() => gc.as_ref().elements().len(),
        Some(crate::memory::Object::String(gc)) => gc.as_ref().data.len(),
        Some(crate::memory::Object::Instance(gc)) => {
            gc.as_ref().slot_len()
        }
        _ => 0,
    }) as u64
}

/// `ProductLoad`, and `MatchPayload` of a boxed enum: word `slot` of a
/// product object, an instance's field or an enum's payload word (a tuple's
/// item). Native code reads it in place ([`product_slots`]); this is the
/// interpreter's read. Anything else, or a slot past the object, is a
/// compiler bug: bail, which panics, as native code's read does.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `addr` is a heap word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_product_load(ctx: *mut NativeCtx, addr: u64, slot: u64) -> u64 {
    let slot = slot as usize;
    let word = match object(addr) {
        Some(crate::memory::Object::Enum(e)) => e.as_ref().payload.get(slot).copied(),
        Some(crate::memory::Object::Instance(gc)) => gc.as_ref().slot(slot),
        _ => None,
    };
    match word {
        Some(v) => v.bits(),
        // SAFETY: the caller passes its live context.
        None => unsafe { bail(ctx) },
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

/// The object a two-word element `[lo, hi]` of a lane array is as one
/// word, as the compiler boxes it (`MirLaneShape`): a two-item tuple when
/// `payload_mask` is `0`, otherwise the enum variant of tag `hi`, whose
/// payload is `lo` (or the empty tuple, under `unit_mask`) when it has one.
fn lane_box(heap: &mut crate::memory::Heap, lo: u64, hi: u64, shape: [u64; 3]) -> u64 {
    use crate::memory::{EnumPayload, ObjEnum, Object};
    let [tag_mask, payload_mask, unit_mask] = shape;
    if payload_mask == 0 {
        let kinds = common::pack_word_kinds([common::WORD_SCALAR, common::WORD_SCALAR]);
        let items = [common::Value::from(lo), common::Value::from(hi)];
        return heap.alloc(ObjEnum::tuple(&items, kinds), Object::Enum).0.addr();
    }
    let tag = hi as u32;
    let bit = 1u64.checked_shl(tag).unwrap_or(0);
    if payload_mask & bit == 0 {
        return heap.immortal_unit_enum(tag).addr();
    }
    let (payload, kind) = if unit_mask & bit != 0 {
        (heap.immortal_empty_tuple().addr(), common::WORD_POINTER)
    } else if tag_mask & bit != 0 {
        (lo, common::WORD_POINTER)
    } else {
        (lo, common::WORD_SCALAR)
    };
    let payload = EnumPayload::one(common::Value::from(payload)).with_kinds(common::pack_word_kinds([kind]));
    heap.alloc(ObjEnum::new(tag, payload), Object::Enum).0.addr()
}

/// The two words of a boxed two-word element (an array of one word per
/// element holds them so): a tuple's items, an enum's payload (`0` for
/// none) and tag.
fn lane_unbox(word: common::Value) -> Option<(u64, u64)> {
    match object(word.bits())? {
        crate::memory::Object::Enum(gc) if gc.as_ref().is_tuple() => match gc.as_ref().elements() {
            [a, b] => Some((a.bits(), b.bits())),
            _ => None,
        },
        crate::memory::Object::Enum(gc) => {
            let e = gc.as_ref();
            Some((e.payload.first().map_or(0, |v| v.bits()), u64::from(e.tag)))
        }
        _ => None,
    }
}

/// `LaneIndex`: element `index` of a lane array (or of an array of one
/// word per element, unboxed), its first word returned and its second in
/// [`NativeCtx::ret_hi`]. Out of bounds traps; anything else: bail.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `addr` is a heap word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_lane_index(ctx: *mut NativeCtx, addr: u64, index: i64) -> u64 {
    let Some(crate::memory::Object::Array(gc)) = object(addr) else {
        return unsafe { bail(ctx) };
    };
    let arr = gc.as_ref();
    let read = if arr.lanes() == 2 {
        usize::try_from(index).ok().and_then(|i| arr.get_lanes(i)).map(|w| Some((w[0].bits(), w[1].bits())))
    } else {
        arr.read_indexed(index, false).map(lane_unbox)
    };
    match read {
        Some(Some((lo, hi))) => {
            unsafe { (*ctx).ret_hi = hi };
            lo
        }
        Some(None) => unsafe { bail(ctx) },
        None => unsafe {
            (*ctx).trap = TRAP_OUT_OF_BOUNDS;
            bail(ctx)
        },
    }
}

/// `LaneStore`: `[lo, hi]` as element `index` of a lane array, or boxed
/// as `shape` says into an array of one word per element.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `addr` is a heap word.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn coil_native_lane_store(
    ctx: *mut NativeCtx,
    addr: u64,
    index: i64,
    lo: u64,
    hi: u64,
    tag_mask: u64,
    payload_mask: u64,
    unit_mask: u64,
) -> u64 {
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    let Some(crate::memory::Object::Array(mut gc)) = object(addr) else {
        return unsafe { bail(ctx) };
    };
    let stored = if gc.as_ref().lanes() == 2 {
        usize::try_from(index).is_ok_and(|i| gc.as_mut().set_lanes(i, &[common::Value::from(lo), common::Value::from(hi)]))
    } else if gc.as_ref().read_indexed(index, false).is_some() {
        let boxed = lane_box(heap, lo, hi, [tag_mask, payload_mask, unit_mask]);
        gc.as_mut().store_indexed(index, common::Value::from(boxed), false);
        unsafe { note_alloc(ctx, heap) };
        true
    } else {
        false
    };
    if !stored {
        unsafe { (*ctx).trap = TRAP_OUT_OF_BOUNDS };
        return unsafe { bail(ctx) };
    }
    1
}

/// `LanePush`: append `[lo, hi]` to a lane array, or boxed as `shape`
/// says to an array of one word per element; returns the array.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `addr` is a heap word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_lane_push(ctx: *mut NativeCtx, addr: u64, lo: u64, hi: u64, tag_mask: u64, payload_mask: u64, unit_mask: u64) -> u64 {
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    let Some(crate::memory::Object::Array(mut gc)) = object(addr) else {
        return unsafe { bail(ctx) };
    };
    if gc.as_ref().lanes() == 2 {
        gc.as_mut().push_lanes(&[common::Value::from(lo), common::Value::from(hi)]);
    } else {
        let boxed = lane_box(heap, lo, hi, [tag_mask, payload_mask, unit_mask]);
        gc.as_mut().push(common::Value::from(boxed));
    }
    heap.account_array(gc.as_mut());
    unsafe { note_alloc(ctx, heap) };
    addr
}

/// `LanesToWords`: a lane array turned into one word per element in
/// place, each element boxed as `shape` says; an array of one word per
/// element is left as it is. Returns the array.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `addr` is a heap word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_lanes_to_words(ctx: *mut NativeCtx, addr: u64, tag_mask: u64, payload_mask: u64, unit_mask: u64) -> u64 {
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    let Some(crate::memory::Object::Array(mut gc)) = object(addr) else {
        return unsafe { bail(ctx) };
    };
    let Some(words) = gc.as_ref().lane_words().filter(|_| gc.as_ref().lanes() == 2).map(<[common::Value]>::to_vec) else {
        return addr;
    };
    // No collection runs inside an allocation, so the boxes made so far
    // stay alive until the array holds them.
    let boxes = words
        .as_chunks::<2>()
        .0
        .iter()
        .map(|w| common::Value::from(lane_box(heap, w[0].bits(), w[1].bits(), [tag_mask, payload_mask, unit_mask])))
        .collect();
    gc.as_mut().lanes_into_words(boxes);
    heap.account_array(gc.as_mut());
    unsafe { note_alloc(ctx, heap) };
    addr
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
    if slot >= gc.as_ref().slot_len() {
        return unsafe { bail(ctx) };
    }
    gc.as_mut().set_slot(slot, common::Value::from(value));
    value
}

/// An array's element words, for native code to index in place: writes
/// `[pointer, length]` to `out` and returns [`crate::memory::VIEW_PLAIN`]
/// for an array that stores words, `0` for anything else (a byte array, a
/// tuple, not an array), which native code then reads through the helpers
/// above. A store view of an array with a card table returns
/// [`crate::memory::VIEW_TRACKED`] and writes the card bytes' address as a
/// third word: loads and stores stay in place, and a store also writes `1`
/// at `cards + (index >> CARD_SHIFT)` (#2687). The cards cover the length,
/// and only a push, a runtime call or a collection grows or clears them,
/// which all drop the view. With
/// `for_store` set, marks the array as possibly holding references, as
/// every runtime store does. The pointer stays valid while native code runs:
/// it never grows or shrinks an array, and storing into a word array never
/// moves its storage.
///
/// # Safety
/// `addr` is a heap word; `out` points at three writable words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_view(addr: u64, for_store: u64, out: *mut u64) -> u64 {
    let Some(crate::memory::Object::Array(mut gc)) = object(addr) else {
        return 0;
    };
    let view = if for_store != 0 {
        let view = gc.as_mut().words_view_for_store();
        gc.note_store_view();
        view
    } else {
        gc.as_ref().words_view_for_load().map(|(p, n)| (p, n, std::ptr::null_mut()))
    };
    match view {
        Some((ptr, len, cards)) => {
            unsafe {
                out.write(ptr as u64);
                out.add(1).write(len as u64);
                out.add(2).write(cards as u64);
            }
            // An array with a card table: native code marks each store's card.
            if cards.is_null() { crate::memory::VIEW_PLAIN } else { crate::memory::VIEW_TRACKED }
        }
        None => 0,
    }
}

/// The element words of lane array `addr` in place, for native code's
/// `LaneIndex` / `LaneStore` (two words per element): writes `(pointer,
/// element count)` to `out` and returns `1`, or `0` when `addr` is not a
/// lane array (one word per element: the lane helpers box and unbox).
/// `for_store` marks the array as a store of a heap payload would.
///
/// # Safety
/// `addr` is a heap word or 0; `out` has room for two words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_lane_view(addr: u64, for_store: u64, out: *mut u64) -> u64 {
    let Some(crate::memory::Object::Array(mut gc)) = object(addr) else {
        return 0;
    };
    let view = if for_store != 0 {
        let view = gc.as_mut().lanes_view();
        gc.note_store_view();
        view
    } else {
        gc.as_ref().lanes_view_for_load()
    };
    match view {
        Some((ptr, n)) => {
            unsafe {
                out.write(ptr as u64);
                out.add(1).write(n as u64);
            }
            1
        }
        None => 0,
    }
}

/// The bytes of the string `addr` names, for `s[i]` read in place: writes
/// `(pointer, length)` to `out`, or `(0, 0)` when `addr` is not a string
/// object itself (a boxed one goes through `string_byte_at`). Strings are
/// never written after they exist, so the view holds until a collection
/// may move the object.
///
/// # Safety
/// `addr` is a heap word or 0; `out` has room for two words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_str_view(addr: u64, out: *mut u64) -> u64 {
    let (ptr, len) = match object(addr) {
        Some(crate::memory::Object::String(gc)) => {
            let b = gc.as_ref().data.as_str().as_bytes();
            (b.as_ptr() as u64, b.len() as u64)
        }
        _ => (0, 0),
    };
    unsafe {
        out.write(ptr);
        out.add(1).write(len);
    }
    1
}

// ---------------------------------------------------------------------------
// Copy-on-write for native code. These allocate (a unique copy) but never
// collect; see [`NativeCtx::allocs`].

/// The heap of `ctx`, or `None` (the caller bails).
unsafe fn heap<'a>(ctx: *mut NativeCtx) -> Option<&'a mut crate::memory::Heap> {
    unsafe { (*ctx).heap.as_mut() }
}

/// Count one allocation; with a collection due, collect at the next
/// safepoint.
unsafe fn note_alloc(ctx: *mut NativeCtx, heap: &crate::memory::Heap) {
    let ctx = unsafe { &mut *ctx };
    ctx.allocs += 1;
    if !heap.gc_is_idle() || heap.should_collect() {
        ctx.gc_due = 1;
    }
}

/// A safepoint found [`NativeCtx::gc_due`]: native code linked its heap
/// words into [`NativeCtx::roots`]; the runtime collects with them pinned.
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

/// The interpreter's fuel ran out at a step it charges: whether the run
/// goes on, with fuel refilled ([`crate::thread::refuel`]). A speculative
/// job runs on slices and stops once cancelled; a step budget or a heap
/// over its cap stops any run.
///
/// # Safety
/// `ctx` is the running call's context.
pub unsafe fn coil_native_refuel(ctx: *mut NativeCtx) -> bool {
    // SAFETY: the running call's context.
    let c = unsafe { &*ctx };
    if c.fuel.is_null() || unsafe { heap(ctx) }.is_some_and(|h| h.over_byte_cap()) {
        return false;
    }
    crate::thread::refuel(c.fuel)
}

/// `--cow-report`: the copies `ctx`'s heap had made, when it counts them.
///
/// # Safety
/// `ctx` is the running call's context.
pub unsafe fn cow_report_mark(ctx: *mut NativeCtx) -> Option<u64> {
    // SAFETY: the running call's context.
    let heap = unsafe { heap(ctx) }?;
    heap.cow_report_on().then(|| heap.cow_copies())
}

/// `--cow-report`: the `Cow` at `site` ran; `mark` is what
/// [`cow_report_mark`] said before it.
///
/// # Safety
/// `ctx` is the running call's context.
pub unsafe fn cow_report_note(ctx: *mut NativeCtx, mark: u64, site: u32) {
    // SAFETY: the running call's context.
    if let Some(heap) = unsafe { heap(ctx) } {
        heap.cow_report_note(mark, site);
    }
}

/// `Cow` without a field: share `word` (`unique == 0`) or return a unique
/// copy of it when it is shared (`unique != 0`).
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `word` is a heap word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_cow(ctx: *mut NativeCtx, word: u64, unique: u64) -> u64 {
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    let v = common::Value::from(word);
    if unique == 0 {
        return match heap.cow_share(v) {
            crate::memory::CowShare::Marked => {
                heap.note_cow_share();
                word
            }
            // A pinned object: the new name takes a copy (native code's
            // inline share calls here only for one).
            crate::memory::CowShare::Copied(copy) => {
                heap.note_cow_share();
                unsafe { note_alloc(ctx, heap) };
                copy.bits()
            }
            crate::memory::CowShare::Untouched => word,
        };
    }
    match heap.cow_unique(v) {
        Some(copy) => {
            unsafe { note_alloc(ctx, heap) };
            copy.bits()
        }
        None => word,
    }
}

/// `CowPin`: whether `word`'s object was pinned (`1` or `0`), and pin it
/// ([`crate::memory::Object::is_pinned`]): a share of it copies from now
/// on. `0` and no effect for anything but a class instance or an array,
/// as a share leaves those. Native code does the same inline.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `word` is a heap word.
pub unsafe fn cow_pin(ctx: *mut NativeCtx, word: u64) -> u64 {
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return 0;
    };
    match heap.find_object_by_addr(common::Value::from(word).heap_addr()) {
        Some(obj @ (crate::memory::Object::Instance(_) | crate::memory::Object::Array(_))) => {
            let was = obj.is_pinned();
            obj.set_pinned(true);
            u64::from(was)
        }
        _ => 0,
    }
}

/// `CowUnpin`: unpin `word`'s object unless `saved` (what the `CowPin`
/// that pinned it said) is nonzero, so a pin an outer region holds stays.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `word` is a heap word.
pub unsafe fn cow_unpin(ctx: *mut NativeCtx, word: u64, saved: u64) {
    if saved != 0 {
        return;
    }
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return;
    };
    if let Some(obj @ (crate::memory::Object::Instance(_) | crate::memory::Object::Array(_))) = heap.find_object_by_addr(common::Value::from(word).heap_addr()) {
        obj.set_pinned(false);
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
// Allocation for native code (`Alloc`, `ArrayPush`), as the runtime's
// `DenseMake` / `DenseMakeObject` / `ArrayPush` do. Same rule as the Cow
// helpers: they allocate and never collect.

/// [`coil_native_alloc`] kinds.
pub use common::native_alloc::{
    ALLOC_ARRAY, ALLOC_CORO, ALLOC_DROP_ENUM, ALLOC_ENUM, ALLOC_FN, ALLOC_LANES, ALLOC_OBJECT, ALLOC_POLY_FN,
};

/// A new object of `kind` from the `n` words at `words`, in declaration
/// order (an enum's payload order):
/// - [`ALLOC_ARRAY`]: `extra` is the element word kind (`0`: none);
/// - [`ALLOC_ENUM`]: `extra` is `tag | kinds << 32`, a tuple's tag
///   [`common::PRODUCT_TAG`];
/// - [`ALLOC_OBJECT`]: `extra` is the type id and `n` the field count
///   (`words` unused: stores fill the fields);
/// - [`ALLOC_FN`]: a function value, as `MakeFn`: `extra` is the entry id
///   `| operand << 32`, the words its captures and filled parameters, then
///   the filled mask;
/// - [`ALLOC_POLY_FN`]: a generic function's value, as `MakePolyFn` /
///   `MakePolyFnCapture`: `extra` is the entry id `| params << 32`, the
///   words its dictionaries (`0`: filled in by the call);
/// - [`ALLOC_DROP_ENUM`]: [`ALLOC_ENUM`] of an enum with a `drop`, as
///   `TagEnumType` stamps it: the last word is its type id;
/// - [`ALLOC_CORO`]: a generator, as `MakeCoro`: `extra` is its body's
///   entry id, the words its arguments;
/// - [`ALLOC_LANES`]: a lane array of two-word elements, the words theirs
///   in order: `extra` is its pointer tag mask.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `words` points at `n` words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_alloc(ctx: *mut NativeCtx, kind: u64, words: *const u64, n: u64, extra: u64) -> u64 {
    use crate::memory::{EnumPayload, ObjArray, ObjEnum, ObjInstance, Object};
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
        ALLOC_LANES => {
            if !n.is_multiple_of(2) {
                return unsafe { bail(ctx) };
            }
            heap.alloc(ObjArray::from_lanes(values, 2, extra), Object::Array).0.addr()
        }
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
                started: false,
                closeable: false,
                native: None,
            };
            heap.alloc(coro, Object::Coroutine).0.addr()
        }
        ALLOC_FN => {
            let op = (extra >> 32) as u32;
            let n_filled = ((op >> 8) & 0xFF) as usize;
            let Some((&mask, rest)) = values.split_last() else {
                return unsafe { bail(ctx) };
            };
            // The words say how many captures: their 8 bits in `op` are the
            // the interpreter's.
            let Some(n_captures) = rest.len().checked_sub(n_filled) else {
                return unsafe { bail(ctx) };
            };
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

/// `Probe`: count one more run of coverage probe `id`. A runtime with no
/// counters (a thread the case spawned) counts nothing.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_probe(ctx: *mut NativeCtx, id: u64) -> u64 {
    // SAFETY: when set, `probes` is the live runtime's.
    if let Some(n) = unsafe { (*ctx).probes.as_mut() }.and_then(|p| p.get_mut(id as usize)) {
        *n = n.wrapping_add(1);
    }
    0
}

/// `ArrayPush`: append `value` to the array `addr` and return `addr`. Not an array: bail, where the runtime panics.
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

/// `STRING idx`: the program string's interned object, as the runtime's
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
    // valid until the runtime's next sweep clears it, as after `STRING`.
    unsafe { *c.string_cache.add(idx as usize) = handle };
    unsafe { note_alloc(ctx, heap) };
    handle.bits()
}

/// `StrEq`: `1` when `a` and `b` are equal as the runtime
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

/// `STRINGIFY`: a new string of `word`, as the runtime writes it. `tag` is the
/// instruction's operand (`common::stringify_operand`, `0`: inspect the
/// word).
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_stringify(ctx: *mut NativeCtx, word: u64, tag: u64) -> u64 {
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    let v = common::Value::from(word);
    let text = match common::stringify_tag(tag as u32) {
        Some(t) => crate::format::stringify_immediate(t, v),
        None => crate::format::stringify_value(heap, v),
    };
    let s = heap.alloc_string(text);
    unsafe { note_alloc(ctx, heap) };
    s.as_ptr() as *mut u8 as u64
}

/// The `n` argument words at `args`.
fn words_slice<'a>(args: *const u64, n: u64) -> &'a [u64] {
    // SAFETY: native code passes `n` words at `args` (none when `n == 0`).
    if n == 0 { &[] } else { unsafe { std::slice::from_raw_parts(args, n as usize) } }
}

/// The strings a format of only `%s` (after the head's) appends: `None` when
/// the format has anything else, the count is off (or over 8), or an operand
/// is not a string (the general renderer handles those).
fn concat_tails<'a>(rest: &str, args: &[u64]) -> Option<([&'a str; 8], usize)> {
    if args.len() > 8 || rest.len() != 2 * args.len() || !rest.as_bytes().chunks(2).all(|c| c == b"%s") {
        return None;
    }
    let mut out = [""; 8];
    for (slot, &w) in out.iter_mut().zip(args) {
        let Some(crate::memory::Object::String(g)) = object(w) else {
            return None;
        };
        // SAFETY: a live string object outlives this call.
        *slot = unsafe { &*std::ptr::from_ref(g.as_ref().data.as_str()) };
    }
    Some((out, args.len()))
}

/// `FORMAT` with the format string `fmt` and the `n` argument words at
/// `args` (in order), as the runtime's handler: a leading `%s` appends the rest
/// to the first argument (in place when that string ends at its buffer's
/// tail), and with no arguments only `%%` is rewritten.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `fmt` is a string object; `args`
/// points at `n` words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_format(ctx: *mut NativeCtx, fmt: u64, args: *const u64, n: u64) -> u64 {
    use crate::memory::{GcData, ObjString, Object};
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
        // String `+` and `join` lower to `"%s%s…"` with string operands: append
        // them straight onto the head, with no format parse or temporary render.
        if let Some(Some(Object::String(gh))) = head.map(|h| object(h.bits()))
            && let Some((tails, count)) = concat_tails(rest, &words_slice(args, n)[1..])
        {
            let joined = match &tails[..count] {
                [one] => gh.as_ref().concat(one),
                many => {
                    let mut tail = String::with_capacity(many.iter().map(|t| t.len()).sum());
                    for t in many {
                        tail.push_str(t);
                    }
                    gh.as_ref().concat(&tail)
                }
            };
            let addr = heap.alloc(joined, Object::String).0.addr();
            unsafe { note_alloc(ctx, heap) };
            return addr;
        }
        let message = crate::format::render_format(rest, || words.next().unwrap_or_default());
        match head {
            Some(head) => {
                let joined = match object(head.bits()) {
                    Some(Object::String(gh)) => gh.as_ref().concat(&message),
                    _ => ObjString::new(crate::format::string_at(heap, head.bits()) + &message),
                };
                heap.alloc(joined, Object::String).0.addr()
            }
            None => heap.alloc_string(message).as_ptr() as *mut u8 as u64,
        }
    };
    unsafe { note_alloc(ctx, heap) };
    addr
}

/// `FreeEarly`: the runtime frees `word` now. With finalizers in the
/// program the free may run `drop`s (a collection may run: native code roots
/// its words around this call then). A `drop` that panicked unwinds with
/// [`TRAP_PANICKED`].
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_free_early(ctx: *mut NativeCtx, word: u64, deep: u64) -> u64 {
    let c = unsafe { &mut *ctx };
    if c.machine.is_null() {
        return unsafe { bail(ctx) };
    }
    if c.has_finalizers != 0 {
        c.host_calls += 1;
        c.gc_epoch += 1;
        let (machine, free) = (c.machine, c.host.free);
        // SAFETY: `machine` is the live machine `host` was made for.
        if !unsafe { free(machine, ctx, word, deep != 0) } {
            let c = unsafe { &mut *ctx };
            c.trap = TRAP_PANICKED;
            return unsafe { bail(ctx) };
        }
        return 0;
    }
    // SAFETY: `machine` is the live machine `host` was made for.
    let _ = unsafe { (c.host.free)(c.machine, ctx, word, deep != 0) };
    0
}

/// `PRINT` of the string `word`.
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
    // SAFETY: `machine` is the live machine `host` was made for.
    unsafe { (c.host.print)(c.machine, gc.as_ref().data.as_str()) };
    0
}

/// `CallIndirect` from native code: call the function value (or code
/// pointer) `callee` on the `n` words at `args`, the last `dicts` of them
/// instance dictionaries, on the runtime. Commits first, as
/// the driver does, with its panic rule.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `args` points at `n` words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_call_value(ctx: *mut NativeCtx, callee: u64, args: *const u64, n: u64, dicts: u64) -> u64 {
    let c = unsafe { &mut *ctx };
    if c.machine.is_null() || dicts > n {
        return unsafe { bail(ctx) };
    }
    let mut few = [common::Value::default(); 16];
    let mut many = Vec::new();
    let words: &mut [common::Value] = if n as usize <= few.len() {
        &mut few[..n as usize]
    } else {
        many.resize(n as usize, common::Value::default());
        &mut many
    };
    for (i, w) in words.iter_mut().enumerate() {
        // SAFETY: `args` holds `n` words.
        *w = common::Value::from(unsafe { *args.add(i) });
    }
    c.host_calls += 1;
    c.gc_epoch += 1;
    let mut panicked = false;
    let (machine, call) = (c.machine, c.host.call_value);
    // SAFETY: `machine` is the live machine `host` was made for.
    let out = unsafe { call(machine, ctx, callee, words, dicts as u32, &mut panicked) };
    let c = unsafe { &mut *ctx };
    if panicked {
        c.trap = TRAP_PANICKED;
        return unsafe { bail(ctx) };
    }
    out
}

/// `CallIndirect` of a plain function value, entered by native code
/// itself as a direct `CALL` is: a code offset, or a function object with
/// no partial arguments, rest parameter or dictionaries, taking exactly the
/// `n` argument words at `args`. Writes the body's words (the captures,
/// then the arguments) to `out` and returns its entry, which native code
/// calls with this context and `out`. `0`: anything else, which native
/// code passes to [`coil_native_call_value`]. Never collects.
///
/// # Safety
/// `ctx` is the call's live context; `args` holds `n` words; `out` has
/// room for [`FN_ENTRY_WORDS`] words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_fn_entry(ctx: *mut NativeCtx, callee: u64, args: *const u64, n: u64, out: *mut u64) -> u64 {
    // SAFETY: as the caller promises.
    unsafe { fn_entry(ctx, callee, args, n, out, false) }
}

/// [`coil_native_fn_entry`] for a call of a two-word result: the entry
/// of the body's two-word twin ([`NativeEntry::twin`]) when it has one,
/// which returns its words as a direct `CALL`'s (`lo`, then
/// [`NativeCtx::ret_hi`]), and `1` in `out[FN_ENTRY_WORDS]`; else the
/// body's own one-word entry and `0` there.
///
/// # Safety
/// As [`coil_native_fn_entry`], with room for one more word at `out`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_fn_entry_pair(ctx: *mut NativeCtx, callee: u64, args: *const u64, n: u64, out: *mut u64) -> u64 {
    // SAFETY: as the caller promises.
    unsafe { fn_entry(ctx, callee, args, n, out, true) }
}

#[inline(always)]
unsafe fn fn_entry(ctx: *mut NativeCtx, callee: u64, args: *const u64, n: u64, out: *mut u64, pair: bool) -> u64 {
    let c = unsafe { &mut *ctx };
    if c.code.is_null() || c.heap.is_null() || c.machine.is_null() || c.entry_data != 0 {
        return 0;
    }
    // SAFETY: the driver keeps the code for the whole run.
    let code = unsafe { &*c.code };
    let n = n as usize;
    // SAFETY: as the caller promises.
    let given = unsafe { std::slice::from_raw_parts(args, n) };
    if pair {
        // SAFETY: as the caller promises.
        unsafe { *out.add(FN_ENTRY_WORDS) = 0 };
    }
    let words = unsafe { std::slice::from_raw_parts_mut(out, FN_ENTRY_WORDS) };
    // Word copies by hand: a call passes a few words, below what a
    // `memcpy` / `memset` call pays for.
    let (pc, len) = if callee < code.code_len() as u64 {
        // A code offset: a function with nothing captured.
        let Some(w) = words.get_mut(..n) else { return 0 };
        for (w, v) in w.iter_mut().zip(given) {
            *w = *v;
        }
        (callee as usize, n)
    } else {
        // SAFETY: the context's heap is the machine's.
        let Some(gc) = unsafe { &*c.heap }.fn_at(callee) else {
            return 0;
        };
        let f = gc.as_ref();
        if f.dicts.is_some() || f.filled_mask != 0 || f.is_rest || f.arity as usize != n {
            return 0;
        }
        let caps = f.captures.len();
        let len = caps + n;
        let Some(w) = words.get_mut(..len) else { return 0 };
        for (w, v) in w.iter_mut().zip(f.captures.iter().map(|v| v.bits()).chain(given.iter().copied())) {
            *w = v;
        }
        (f.entry as usize, len)
    };
    let Some(mut entry) = code.entry_ref(pc) else { return 0 };
    // The twin takes the same words.
    let mut tag = 0;
    if pair
        && entry.twin != 0
        && let Some(twin) = code.entry_ref(entry.twin as usize - 1)
        && twin.ret_words == 2
    {
        entry = twin;
        tag = 1;
    }
    // A generator's body is entered by a resume; a body without machine
    // code by the runtime.
    if entry.generator || entry.data != 0 || len < entry.reads as usize || entry.arity as usize > FN_ENTRY_WORDS {
        return 0;
    }
    // The words past `len` up to its arity are `0`, as the runtime passes
    // them; words past its arity it does not read.
    for w in words.iter_mut().take(entry.arity as usize).skip(len) {
        *w = 0;
    }
    c.host_calls += 1;
    c.gc_epoch += 1;
    if pair {
        // SAFETY: the caller gave room for the flag past the words.
        unsafe { *out.add(FN_ENTRY_WORDS) = tag };
    }
    entry.entry as usize as u64
}

/// The words [`coil_native_fn_entry`] writes at most.
pub const FN_ENTRY_WORDS: usize = 16;

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
    c.host_calls += 1;
    c.gc_epoch += 1;
    let mut panicked = false;
    let (machine, resume) = (c.machine, c.host.resume);
    let send = (has_send != 0).then_some(send);
    // SAFETY: `machine` is the live machine `host` was made for.
    let out = unsafe { resume(machine, ctx, handle, send, &mut panicked) };
    let c = unsafe { &mut *ctx };
    if panicked {
        c.trap = TRAP_PANICKED;
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
                let call = crate::ffi::HostCall::of(n.as_ref());
                DirectHost { name: n.name().to_string(), func: call.func, guarded: call.guarded, collect: call.collect, on_machine: call.on_machine }
            })
            .collect()
    })
}

/// One entry of [`direct_host_natives`] (what [`crate::ffi::HostCall`]
/// says of a standard native, with its name).
pub struct DirectHost {
    pub name: String,
    /// The plain function (`None`: the native needs the runtime).
    pub func: Option<crate::HostValueFn>,
    /// It takes a `Stream` first, which must not be unsent.
    pub guarded: bool,
    /// `gc_collect`: a full collection on the runtime ([`NativeHost::gc`]),
    /// with native code's words rooted around the call.
    pub collect: bool,
    /// An ordinary native that needs the runtime (it may park the task, open
    /// descriptors, ...), a task op, a `region` hook (a minor collection)
    /// or the finalizer registry's `gc_register_finalizer`:
    /// native code runs it through a host-call stub
    /// ([`host_stubs`]) in a nested interpreter execution, as a runtime call.
    pub on_machine: bool,
}

/// Arities a host-call stub exists for (the words after the native id).
pub(crate) const HOST_STUB_ARITIES: usize = 16;

/// `HostInvoke` of the direct host native `id` ([`direct_host_natives`])
/// on the `n` words at `args`, constructing enums in `layout`
/// (`common::HOST_ENUM_LAYOUT_*`), as the runtime's direct path does.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `args` points at `n` words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_host(ctx: *mut NativeCtx, id: u64, layout: u64, args: *const u64, n: u64) -> u64 {
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    let c = unsafe { &mut *ctx };
    // The runtime's table decides what runs: a host may have replaced a
    // native (the compile-time sandbox refuses most of them), and an AOT
    // executable links only the natives its program calls.
    // SAFETY: when set, `natives` is the live runtime's.
    let Some(natives) = (unsafe { c.natives.as_ref() }) else {
        return unsafe { bail(ctx) };
    };
    let Some(host) = natives.host_call(id as usize) else {
        return unsafe { bail(ctx) };
    };
    if host.collect {
        if c.machine.is_null() {
            return unsafe { bail(ctx) };
        }
        c.host_calls += 1;
        c.gc_epoch += 1;
        let mut panicked = false;
        // SAFETY: `machine` is the live machine `host` was made for.
        let freed = unsafe { (c.host.gc)(c.machine, ctx, &mut panicked) };
        if panicked {
            let c = unsafe { &mut *ctx };
            c.trap = TRAP_PANICKED;
            return unsafe { bail(ctx) };
        }
        return freed;
    }
    let f = match host.func {
        Some(f) if !host.on_machine => f,
        _ if host.on_machine || host.func.is_some() => {
            crate::host_enum::take_pair_tag();
            return match unsafe { host_invoke(ctx, id, layout, args, n) } {
                Some(out) if layout as u32 == common::HOST_ENUM_LAYOUT_PAIR => unsafe { split_pair(ctx, out) },
                Some(out) => out,
                None => unsafe { bail(ctx) },
            };
        }
        _ => return unsafe { bail(ctx) },
    };
    if n > 16 {
        return unsafe { bail(ctx) };
    }
    // SAFETY: `args` holds `n` words, and a `Value` is one word.
    let args = unsafe { std::slice::from_raw_parts(args.cast::<common::Value>(), n as usize) };
    if host.guarded && args.first().is_some_and(|&s| crate::io::is_unsent_stream(heap, s)) {
        c.trap = TRAP_MESSAGE;
        // SAFETY: `call` set `message` for the call.
        unsafe { *c.message = format!("{}: {}", natives.get_by_id(id as usize).map_or("?", |n| n.name()), crate::io::UNSENT_STREAM_PANIC) };
        return unsafe { bail(ctx) };
    }
    let live_before = heap.live_object_count();
    let layout = crate::host_enum::HostEnumLayout::from_u32(layout as u32);
    if layout == crate::host_enum::HostEnumLayout::Pair {
        crate::host_enum::take_pair_tag();
    }
    let v = crate::host_enum::in_host_enum_layout(layout, || f(heap, args));
    // The runtime checks for a collection once the call returns.
    let made = heap.live_object_count().saturating_sub(live_before) as u64;
    if made != 0 {
        c.allocs += made - 1;
        unsafe { note_alloc(ctx, heap) };
    }
    if layout == crate::host_enum::HostEnumLayout::Pair {
        return unsafe { split_pair(ctx, v.bits()) };
    }
    v.bits()
}

/// A two-word host result ([`common::HOST_ENUM_LAYOUT_PAIR`]): its payload
/// word returned, its tag left in [`NativeCtx::ret_hi`].
///
/// # Safety
/// `ctx` is the call's live [`NativeCtx`].
unsafe fn split_pair(ctx: *mut NativeCtx, word: u64) -> u64 {
    let Some(heap) = (unsafe { heap(ctx) }) else {
        return unsafe { bail(ctx) };
    };
    match crate::host_enum::split_pair_result(heap, common::Value::from(word)) {
        Some((lo, tag)) => {
            unsafe { (*ctx).ret_hi = tag };
            lo
        }
        None => unsafe { bail(ctx) },
    }
}

/// `Yield` in a generator's body: suspend its fiber with `value` (the
/// resumer takes the frame's root words into the generator meanwhile and
/// puts them back, forwarded, before it resumes); the word the resumer
/// sends comes back.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`], on a generator's fiber.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_yield(ctx: *mut NativeCtx, value: u64) -> u64 {
    if parks() {
        let c = unsafe { &mut *ctx };
        c.park = PARK_YIELD;
        c.park_word = value;
        return unsafe { bail(ctx) };
    }
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
/// `ObjCoroutine` while it is suspended (`gen_fiber.rs`).
#[cfg_attr(target_family = "wasm", allow(dead_code))]
pub struct GenFiber {
    #[cfg(not(target_family = "wasm"))]
    pub(crate) coro: Option<GenCoro>,
    pub(crate) stack_limit: usize,
    pub(crate) yielder: *const (),
    /// The context of the call suspended at a yield (its root words are
    /// the generator's saved words meanwhile).
    pub(crate) parked: *mut NativeCtx,
    /// The call, where it parks ([`parks`]) instead of holding a fiber.
    pub(crate) call: Option<ParkedCall>,
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
/// ([`NativeHost::ffi`]), with the panic rules of a host call.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `args` points at `n` words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_ffi(ctx: *mut NativeCtx, op: u64, operand: u64, args: *const u64, n: u64) -> u64 {
    let ffi = unsafe { (*ctx).host.ffi };
    unsafe { host_on_machine(ctx, ffi, op, operand, args, n) }
}

/// [`coil_native_host`] of an ordinary native that is no plain function:
/// the machine runs it in Rust ([`NativeHost::invoke`]), no interpreted body.
/// `None`: it is no ordinary native.
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
    // A wait lets other tasks run, and they may collect.
    c.gc_epoch += 1;
    let (machine, invoke) = (c.machine, c.host.invoke);
    // SAFETY: `machine` is the live machine `host` was made for.
    let out = unsafe { invoke(machine, ctx, id, layout, words) };
    let c = unsafe { &mut *ctx };
    match out {
        None => {
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
            c.trap = TRAP_PANICKED;
            Some(unsafe { bail(ctx) })
        }
    }
}

/// Run a host entry on the machine, with the panic rules of a call.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `args` points at `n` words.
unsafe fn host_on_machine(ctx: *mut NativeCtx, host: MachineHostFn, id: u64, layout: u64, args: *const u64, n: u64) -> u64 {
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
    // It may allocate (the call reports `allocated`).
    c.host_calls += 1;
    c.gc_epoch += 1;
    let mut panicked = false;
    let machine = c.machine;
    // SAFETY: `machine` is the live machine `host` was made for.
    let out = unsafe { host(machine, ctx, id, layout, words, &mut panicked) };
    let c = unsafe { &mut *ctx };
    let Some(out) = out else {
        // Nothing ran.
        return unsafe { bail(ctx) };
    };
    if panicked {
        c.trap = TRAP_PANICKED;
        return unsafe { bail(ctx) };
    }
    out
}

/// A frame with `defer`s on its way out: after a panic, keep its
/// cleanup pad `func` and a copy of its `n` homes at `homes` (`masks`, at
/// [`mask_words`] words: the heap words among them) for the runtime to run
/// ([`NativeCtx::pads`]).
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `homes` points at `n` words and
/// `masks` at the mask words for them.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_defer(ctx: *mut NativeCtx, func: u64, homes: *const u64, n: u64, masks: *const u64) {
    if func == 0 {
        return;
    }
    // SAFETY: the backend passes a function with the `NativePadFn` ABI.
    let func = unsafe { std::mem::transmute::<usize, NativePadFn>(func as usize) };
    let words = mask_words((n & !ROOTS_AMBIGUOUS) as usize);
    // SAFETY: the backend passes that many mask words.
    let masks = unsafe { std::slice::from_raw_parts(masks, words) };
    // SAFETY: as this function's.
    unsafe { defer_pad(ctx, func, 0, homes, n, masks) };
}

/// [`coil_native_defer`] of a pad that reads `data` from
/// [`NativeCtx::entry_data`].
///
/// # Safety
/// As [`coil_native_defer`].
pub unsafe fn defer_pad(ctx: *mut NativeCtx, func: NativePadFn, data: usize, homes: *const u64, n: u64, masks: &[u64]) {
    let c = unsafe { &mut *ctx };
    if c.bail != BAIL_PANIC || c.pads.is_null() {
        return;
    }
    // `n` may carry `ROOTS_AMBIGUOUS` (an opaque body's `int` homes).
    let (flags, n) = (n & ROOTS_AMBIGUOUS, n & !ROOTS_AMBIGUOUS);
    debug_assert_eq!(masks.len(), mask_words(n as usize));
    let mut rec = vec![0, n | ROOTS_MASKED | flags];
    rec.extend_from_slice(masks);
    // SAFETY: `homes` holds `n` words.
    rec.extend((0..n as usize).map(|i| unsafe { *homes.add(i) }));
    unsafe { (*c.pads).push(NativePad { func, homes: rec, data }) };
}

/// Run the cleanup pads of a native panic, innermost first, on `env`'s
/// machine. Each pad's homes,
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
        let mut ctx = unsafe { NativeCtx::with_heap(stack_limit_at(sp), env.heap) };
        ctx.machine = env.machine;
        ctx.host = env.host;
        ctx.fuel = env.fuel;
        ctx.natives = env.natives;
        ctx.probes = env.probes;
        ctx.strings = env.strings;
        ctx.string_cache = env.string_cache;
        ctx.strings_len = env.strings_len as u64;
        ctx.has_finalizers = u64::from(env.has_finalizers);
        ctx.statics = env.statics;
        ctx.statics_len = env.statics_len as u64;
        let mut trace = Vec::new();
        ctx.trace = &mut trace;
        let mut message = String::new();
        ctx.message = &mut message;
        // The last record links all before it.
        let last = pads.len() - 1;
        ctx.roots = pads[last].homes.as_mut_ptr() as u64;
        let mut inner = Vec::new();
        ctx.pads = &mut inner;
        ctx.entry_data = pads[i].data as u64;
        let words = mask_words((pads[i].homes[1] & !(ROOTS_MASKED | ROOTS_AMBIGUOUS)) as usize);
        let homes = pads[i].homes[2 + words..].as_ptr();
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
/// the call (`0`: a `drop` that ran already).
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

/// `Panic` of the string `msg`: a native panic with that message
/// ([`TRAP_EXPLICIT`]).
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

/// A `ByteScan` or `VecLoop` native code cannot reduce to lane code, run
/// by the runtime's lane kernels (`lanes_on_frame`). `kind` bit 0: a
/// loop (else a scan). `body` holds `nbody`
/// words `opcode | operand << 32`, `slots` the frame's `nslots` words.
/// Returns a scan's stop; a trap panics with the runtime's message.
///
/// # Safety
/// `ctx` is the call's [`NativeCtx`]; `body` and `slots` point at
/// `nbody` and `nslots` words.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_native_vector(ctx: *mut NativeCtx, kind: u64, flags: u64, body: *const u64, nbody: u64, slots: *mut u64, nslots: u64, from: u64, bound: u64) -> u64 {
    let c = unsafe { &mut *ctx };
    // A hit-dense scan re-enters here once per hit, so decode only the
    // `nbody` words (no 128-word fill per call).
    let mut words = [std::mem::MaybeUninit::<common::LaneWord>::uninit(); 128];
    let n = nbody as usize;
    if n > words.len() || c.heap.is_null() {
        return unsafe { bail(ctx) };
    }
    for (i, w) in words[..n].iter_mut().enumerate() {
        // SAFETY: `body` holds `nbody` words.
        let Some(word) = common::LaneWord::from_bits(unsafe { *body.add(i) }) else {
            return unsafe { bail(ctx) };
        };
        w.write(word);
    }
    // SAFETY: the loop above wrote the first `n` words.
    let code: &[common::LaneWord] = unsafe { std::slice::from_raw_parts(words.as_ptr().cast(), n) };
    // SAFETY: `slots` holds `nslots` words.
    let frame = unsafe { std::slice::from_raw_parts_mut(slots, nslots as usize) };
    // SAFETY: `call` set `heap` to the machine's live heap.
    let heap = unsafe { &mut *c.heap };
    match crate::lanes::lanes_on_frame(heap, kind & 1 == 0, flags as u8, code, frame, from as i64, bound as i64) {
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
        ("coil_native_product_load", coil_native_product_load as *const u8),
        ("coil_native_store_index", coil_native_store_index as *const u8),
        ("coil_native_set_field", coil_native_set_field as *const u8),
        ("coil_native_view", coil_native_view as *const u8),
        ("coil_native_str_view", coil_native_str_view as *const u8),
        ("coil_native_fn_entry", coil_native_fn_entry as *const u8),
        ("coil_native_fn_entry_pair", coil_native_fn_entry_pair as *const u8),
        ("coil_native_cow", coil_native_cow as *const u8),
        ("coil_native_cow_field", coil_native_cow_field as *const u8),
        ("coil_native_alloc", coil_native_alloc as *const u8),
        ("coil_native_array_push", coil_native_array_push as *const u8),
        ("coil_native_lane_index", coil_native_lane_index as *const u8),
        ("coil_native_lane_store", coil_native_lane_store as *const u8),
        ("coil_native_lane_push", coil_native_lane_push as *const u8),
        ("coil_native_lanes_to_words", coil_native_lanes_to_words as *const u8),
        ("coil_native_lane_view", coil_native_lane_view as *const u8),
        ("coil_native_probe", coil_native_probe as *const u8),
        ("coil_native_free_early", coil_native_free_early as *const u8),
        ("coil_native_format", coil_native_format as *const u8),
        ("coil_native_stringify", coil_native_stringify as *const u8),
        ("coil_native_eq", coil_native_eq as *const u8),
        ("coil_native_string", coil_native_string as *const u8),
        ("coil_native_enum_tag", coil_native_enum_tag as *const u8),
        ("coil_native_unwind", coil_native_unwind as *const u8),
        ("coil_native_print", coil_native_print as *const u8),
        ("coil_native_host", coil_native_host as *const u8),
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

const HELPERS: usize = 41;

#[cfg(test)]
mod pin_tests {
    use super::*;

    /// The interpreter's `CowPin` / `CowUnpin` ([`cow_pin`], [`cow_unpin`]):
    /// the pin nests, and a share between them copies.
    #[test]
    fn the_pin_helpers_nest_and_a_pinned_share_copies() {
        let mut heap = crate::memory::Heap::default();
        let (obj, _) = heap.alloc(crate::memory::ObjInstance::with_slots(5, vec![common::Value::from(1i64)]), crate::memory::Object::Instance);
        let mut ctx = NativeCtx::new(0);
        ctx.heap = &mut heap;
        let w = obj.addr();
        // SAFETY: `ctx` names a live heap; `w` is one of its objects.
        unsafe {
            let outer = cow_pin(&mut ctx, w);
            let inner = cow_pin(&mut ctx, w);
            assert_eq!((outer, inner), (0, 1));
            let copy = coil_native_cow(&mut ctx, w, 0);
            assert_ne!(copy, w, "a share of a pinned object copies it");
            assert!(!obj.is_shared());
            cow_unpin(&mut ctx, w, inner);
            assert!(obj.is_pinned(), "an inner unpin leaves the outer pin");
            cow_unpin(&mut ctx, w, outer);
            assert!(!obj.is_pinned());
            assert_eq!(coil_native_cow(&mut ctx, w, 0), w, "unpinned, a share marks it");
            assert!(obj.is_shared());
            assert_eq!(cow_pin(&mut ctx, 0), 0, "no object: no pin");
        }
    }
}
