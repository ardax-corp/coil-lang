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
}

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
            if target & UNDO_PUSH != 0 {
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

/// What a native call gave back: its result words and whether it
/// allocated (the VM then checks for a collection).
pub(crate) struct NativeResult {
    pub(crate) lo: u64,
    pub(crate) hi: Option<u64>,
    pub(crate) allocated: bool,
}

/// Run `entry` with `args`. `None` when it bailed.
#[inline(never)]
pub(crate) fn call(
    entry: &NativeEntry,
    args: &[u64],
    heap: &mut crate::memory::Heap,
    strings: &[String],
    string_cache: &mut [common::Value],
) -> Option<NativeResult> {
    let marker = 0u8;
    let sp = std::ptr::addr_of!(marker) as u64;
    let mut undo = Vec::new();
    let mut ctx = NativeCtx::with_heap(sp.saturating_sub(NATIVE_STACK_BUDGET), &mut undo, heap);
    ctx.strings = strings.as_ptr();
    ctx.string_cache = string_cache.as_mut_ptr();
    ctx.strings_len = strings.len().min(string_cache.len()) as u64;
    // SAFETY: the backend compiled `entry` for exactly `arity` argument
    // words, which `args` holds; `ctx` outlives the call.
    let out = unsafe { (entry.entry)(&mut ctx, args.as_ptr()) };
    if ctx.bail == 0 {
        let hi = (entry.ret_words == 2).then_some(ctx.ret_hi);
        return Some(NativeResult { lo: out, hi, allocated: ctx.allocs != 0 });
    }
    ctx.undo();
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
    if ctx.allocs >= NATIVE_ALLOC_CAP && heap.should_collect() {
        ctx.bail = 1;
    }
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
