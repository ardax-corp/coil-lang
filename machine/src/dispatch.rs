//! G1 (COI-374): outlined hot-op dispatch vs the giant `Machine::execute` match.
//!
//! Stable Rust has no guaranteed tail calls (`become` is nightly), so this
//! path cannot use musttail. A 256-entry fn-pointer trampoline is the portable
//! stand-in; `hotmatch` is a compact-match control in the same outlined function.
//!
//! Select at process start with `COIL_THREADED_DISPATCH`:
//! - `0` / `match` — existing giant match (A/B baseline)
//! - `1` / `table` / unset — fn-pointer trampoline (default)
//! - `2` / `hotmatch` — compact match over the same hot subset
//!
//! `COIL_THREADED_CALL=1` and `COIL_THREADED_RETURN=1` together thread
//! `CALL`/`TailCall`/`RETURN`/imm-slot fuses (A/B; default off — fib/tak lose).
//!
//! Debugger-attached runs stay on the giant match so per-op stops still fire.

use std::cell::Cell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use common::{
    ArchivedByte as Byte, ArchivedInstruction as Instruction, ArrayVec, Value, likely, promise,
    unlikely,
};

use super::{FramePins, prefetch_code, resolve_dense_index_object_in, set_jump_target};
use crate::{Frame, Heap, Member, Object, Stack};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Mode {
    Match,
    Table,
    HotMatch,
}

thread_local! {
    static MODE_OVERRIDE: Cell<Option<Mode>> = const { Cell::new(None) };
}

pub(super) fn parse_mode(raw: &str) -> Mode {
    let v = raw.trim();
    if v == "0" || v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("match") {
        Mode::Match
    } else if v == "2" || v.eq_ignore_ascii_case("hotmatch") {
        Mode::HotMatch
    } else {
        Mode::Table
    }
}

pub(super) fn mode() -> Mode {
    if let Some(over) = MODE_OVERRIDE.with(|c| c.get()) {
        return over;
    }
    static CACHED: OnceLock<Mode> = OnceLock::new();
    *CACHED.get_or_init(|| {
        refresh_opt_call_return_hot();
        match std::env::var("COIL_THREADED_DISPATCH") {
            Ok(v) => parse_mode(&v),
            Err(_) => Mode::Table,
        }
    })
}

#[cfg(test)]
pub(super) fn override_mode(mode: Option<Mode>) {
    MODE_OVERRIDE.with(|c| c.set(mode));
    refresh_opt_call_return_hot();
}

fn env_flag_enabled(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(v) => {
            let v = v.trim();
            !(v == "0"
                || v.eq_ignore_ascii_case("off")
                || v.eq_ignore_ascii_case("match")
                || v.eq_ignore_ascii_case("no"))
        }
        Err(_) => default,
    }
}

/// `CALL` / `TailCall` on the trampoline. Default off: fib/tak lose to match.
pub(super) fn call_is_hot() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| env_flag_enabled("COIL_THREADED_CALL", false))
}

/// `RETURN` and fused returns. Default off (same A/B as CALL).
pub(super) fn return_is_hot() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| env_flag_enabled("COIL_THREADED_RETURN", false))
}

trait CallFrames {
    fn rewrite_call(&mut self, caller_ip: usize, callee_sp: usize);
    fn rewrite_indirect_call(&mut self, return_ip: usize, callee_sp: usize);
    fn top_sp(&self) -> usize;
    fn caller_ip_sp(&self) -> (usize, usize);
    fn pop_sp(&mut self) -> usize;
    fn len(&self) -> usize;
}

impl<const S: usize> CallFrames for ArrayVec<Frame, S> {
    #[inline(always)]
    fn rewrite_call(&mut self, caller_ip: usize, callee_sp: usize) {
        self.rewrite_top_and_push(
            |caller| caller.seek(caller_ip),
            |frame| frame.set(callee_sp),
        );
    }

    #[inline(always)]
    fn rewrite_indirect_call(&mut self, return_ip: usize, callee_sp: usize) {
        self.rewrite_top_and_push(
            |caller| caller.seek(return_ip),
            |frame| frame.set(callee_sp),
        );
    }

    #[inline(always)]
    fn top_sp(&self) -> usize {
        self.get().get()
    }

    #[inline(always)]
    fn caller_ip_sp(&self) -> (usize, usize) {
        let f = self.get();
        (f.tell(), f.get())
    }

    #[inline(always)]
    fn pop_sp(&mut self) -> usize {
        self.pop().get()
    }

    #[inline(always)]
    fn len(&self) -> usize {
        ArrayVec::len(self)
    }
}

struct HotExtra<'a> {
    frames: &'a mut dyn CallFrames,
    frames_len: usize,
    frame_pins: &'a mut Vec<FramePins>,
    dense_obj_addr: &'a mut u64,
    dense_obj: &'a mut Option<Object>,
    finalizer_pcs: &'a std::collections::HashSet<u32, crate::AddrHashBuilder>,
    nested_depth: &'a mut u32,
    nested_frame_depths: &'a mut Vec<usize>,
    nested_return: &'a mut Option<Value>,
    resume_stack: &'a mut Vec<super::ResumeCtx>,
    io_reactor: &'a std::sync::Arc<crate::io_reactor::IoReactor>,
    execute_done: Option<bool>,
}

struct HotCtx<'a, 'e> {
    stack: &'a mut Stack<Value>,
    sp: usize,
    ip: usize,
    code: &'a [Byte],
    constants: &'a [u64],
    heap: &'a mut Heap,
    stack_cap: usize,
    panic_msg: Option<&'static str>,
    extra: &'e mut HotExtra<'a>,
}

type Handler = fn(&mut HotCtx<'_, '_>, Byte);

const fn hot_bit(op: Instruction) -> (usize, u64) {
    let i = op as u8 as usize;
    (i >> 6, 1u64 << (i & 63))
}

const fn or_hot(mut bits: [u64; 4], op: Instruction) -> [u64; 4] {
    let (word, mask) = hot_bit(op);
    bits[word] |= mask;
    bits
}

/// Default trampoline ops (not CALL/RETURN/imm). `.rodata` so fib never loads
/// the 2 KiB handler table — that peek was the ~14% identical-bytecode tax.
const ALWAYS_HOT: [u64; 4] = {
    let mut b = [0u64; 4];
    b = or_hot(b, Instruction::DenseBin);
    b = or_hot(b, Instruction::DenseBin2);
    b = or_hot(b, Instruction::DenseBinJmpf);
    b = or_hot(b, Instruction::DenseIndexJmpf);
    b = or_hot(b, Instruction::DenseCmp);
    b = or_hot(b, Instruction::DenseConst);
    b = or_hot(b, Instruction::DenseMove);
    b = or_hot(b, Instruction::DenseCast);
    b = or_hot(b, Instruction::DenseUnary);
    b = or_hot(b, Instruction::DenseIndex);
    b = or_hot(b, Instruction::DenseStoreIndex);
    b = or_hot(b, Instruction::DenseArrayLen);
    b = or_hot(b, Instruction::DenseFieldLoad);
    b = or_hot(b, Instruction::DenseFieldStore);
    b = or_hot(b, Instruction::JMP);
    b = or_hot(b, Instruction::JMPF);
    b = or_hot(b, Instruction::JMPT);
    b = or_hot(b, Instruction::BinSlotSlotJmpf);
    b = or_hot(b, Instruction::BinSlotSlotJmpt);
    b = or_hot(b, Instruction::CmpJmpf);
    b = or_hot(b, Instruction::CmpJmpt);
    b = or_hot(b, Instruction::LogNotJmpf);
    b = or_hot(b, Instruction::LogNotJmpt);
    b = or_hot(b, Instruction::BinSlotSlotStore);
    b
};

const OPT_HOT: [u64; 4] = {
    let mut b = [0u64; 4];
    b = or_hot(b, Instruction::BinSlotImm);
    b = or_hot(b, Instruction::BinSlotImmStore);
    b = or_hot(b, Instruction::BinSlotImmJmpf);
    b = or_hot(b, Instruction::BinSlotImmJmpt);
    b = or_hot(b, Instruction::CALL);
    b = or_hot(b, Instruction::TailCall);
    b = or_hot(b, Instruction::RETURN);
    b = or_hot(b, Instruction::ConstReturnImm);
    b = or_hot(b, Instruction::LoadReturnSlot);
    b = or_hot(b, Instruction::BinReturn);
    b
};

static OPT_CALL_RETURN_HOT: AtomicBool = AtomicBool::new(false);

fn refresh_opt_call_return_hot() {
    OPT_CALL_RETURN_HOT.store(call_is_hot() && return_is_hot(), Ordering::Relaxed);
}

#[inline(always)]
pub(super) fn is_hot(bc: Instruction) -> bool {
    let i = bc as u8 as usize;
    if (ALWAYS_HOT[i >> 6] >> (i & 63)) & 1 != 0 {
        return true;
    }
    if !OPT_CALL_RETURN_HOT.load(Ordering::Relaxed) {
        return false;
    }
    (OPT_HOT[i >> 6] >> (i & 63)) & 1 != 0
}

#[inline(always)]
pub(super) fn dense_bin(stack: &mut Stack<Value>, sp: usize, opcode: &Byte, stack_cap: usize) {
    let (kind, dest, lhs, rhs) = opcode.dense_abc_parts();
    promise!(sp + dest < stack_cap);
    promise!(sp + lhs < stack_cap);
    promise!(sp + rhs < stack_cap);
    let va = stack[sp + lhs];
    let vb = stack[sp + rhs];
    stack[sp + dest] = crate::dense::eval_bin(kind, va, vb);
}

#[inline(always)]
fn take_code_word(ctx: &mut HotCtx<'_, '_>) -> Byte {
    promise!(ctx.ip < ctx.code.len());
    let word = copy_byte(ctx.code, ctx.ip);
    ctx.ip += 1;
    super::prefetch_code(ctx.code, ctx.ip);
    word
}

#[inline(always)]
pub(super) fn dense_bin2(stack: &mut Stack<Value>, sp: usize, first: &Byte, second: &Byte, stack_cap: usize) {
    dense_bin(stack, sp, first, stack_cap);
    dense_bin(stack, sp, second, stack_cap);
}

/// Payload of [`Instruction::DenseBinJmpf`]: fused slot compare-jump (or twin).
#[inline(always)]
pub(super) fn dense_bin_jmp_tail(
    stack: &mut Stack<Value>,
    sp: usize,
    tail: &Byte,
    constants: &[u64],
    heap: &Heap,
    stack_cap: usize,
) -> Option<usize> {
    match *tail.bytecode() {
        Instruction::BinSlotSlotJmpf => {
            bin_slot_slot_jmp(stack, sp, tail, constants, heap, stack_cap, false)
        }
        Instruction::BinSlotSlotJmpt => {
            bin_slot_slot_jmp(stack, sp, tail, constants, heap, stack_cap, true)
        }
        Instruction::BinSlotImmJmpf => {
            bin_slot_imm_jmp(stack, sp, tail, constants, heap, stack_cap, false)
        }
        Instruction::BinSlotImmJmpt => {
            bin_slot_imm_jmp(stack, sp, tail, constants, heap, stack_cap, true)
        }
        _ => None,
    }
}

#[inline(always)]
pub(super) fn dense_cmp(stack: &mut Stack<Value>, sp: usize, opcode: &Byte, stack_cap: usize) {
    let (kind, dest, lhs, rhs) = opcode.dense_abc_parts();
    promise!(sp + dest < stack_cap);
    promise!(sp + lhs < stack_cap);
    promise!(sp + rhs < stack_cap);
    let va = stack[sp + lhs];
    let vb = stack[sp + rhs];
    stack[sp + dest] = crate::dense::eval_cmp(kind, va, vb);
}

#[inline(always)]
pub(super) fn dense_const(
    stack: &mut Stack<Value>,
    sp: usize,
    opcode: &Byte,
    constants: &[u64],
    stack_cap: usize,
) {
    let (ty, dest, payload, is_pool) = opcode.dense_const_parts();
    promise!(sp + dest < stack_cap);
    let raw = if is_pool {
        let idx = payload as usize;
        promise!(idx < constants.len());
        unsafe { *constants.get_unchecked(idx) }
    } else {
        payload as i16 as i64 as u64
    };
    stack[sp + dest] = crate::dense::eval_const(ty, raw);
}

#[inline(always)]
pub(super) fn dense_move(stack: &mut Stack<Value>, sp: usize, opcode: &Byte, stack_cap: usize) {
    let (dest, src) = opcode.dense_move_parts();
    promise!(sp + dest < stack_cap);
    promise!(sp + src < stack_cap);
    stack[sp + dest] = stack[sp + src];
}

#[inline(always)]
pub(super) fn dense_cast(stack: &mut Stack<Value>, sp: usize, opcode: &Byte, stack_cap: usize) {
    let (kind, dest, src) = opcode.dense_unary_parts();
    promise!(sp + dest < stack_cap);
    promise!(sp + src < stack_cap);
    stack[sp + dest] = crate::dense::eval_cast(kind, stack[sp + src]);
}

#[inline(always)]
pub(super) fn dense_unary(stack: &mut Stack<Value>, sp: usize, opcode: &Byte, stack_cap: usize) {
    let (kind, dest, src) = opcode.dense_unary_parts();
    promise!(sp + dest < stack_cap);
    promise!(sp + src < stack_cap);
    stack[sp + dest] = crate::dense::eval_unary(kind, stack[sp + src]);
}

#[inline(always)]
pub(super) fn load(stack: &mut Stack<Value>, sp: usize, opcode: &Byte, stack_cap: usize) {
    let count = opcode.load_store_count();
    for i in 0..count {
        let slot = opcode.load_store_slot_at(i) as usize;
        promise!(sp + slot < stack_cap);
        stack.push(stack[sp + slot]);
    }
}

#[inline(always)]
pub(super) fn store(stack: &mut Stack<Value>, sp: usize, opcode: &Byte, stack_cap: usize) {
    let count = opcode.load_store_count();
    let mut max_slot = sp;
    for i in 0..count {
        let slot = sp + opcode.load_store_slot_at(i) as usize;
        promise!(slot < stack_cap);
        max_slot = max_slot.max(slot);
        let val = stack.pop();
        stack[slot] = val;
    }
    let need = max_slot + 1;
    if stack.tell() < need {
        stack.seek(need);
    }
}

#[inline(always)]
pub(super) fn seek(stack: &mut Stack<Value>, sp: usize, opcode: &Byte, stack_cap: usize) {
    let slot = opcode.operand_u32() as usize;
    let abs = sp + slot;
    promise!(abs <= stack_cap);
    stack.seek(abs);
}

#[inline(always)]
pub(super) fn bin_slot_imm(
    stack: &mut Stack<Value>,
    sp: usize,
    opcode: &Byte,
    heap: &Heap,
    stack_cap: usize,
) {
    let (op, slot, imm) = opcode.bin_slot_imm_parts();
    promise!(sp + slot < stack_cap);
    let lhs = stack[sp + slot];
    let rhs = Value::from(imm);
    stack.push(crate::fused::eval_bin(op, lhs, rhs, heap));
}

#[inline(always)]
pub(super) fn bin_slot_imm_store(
    stack: &mut Stack<Value>,
    sp: usize,
    opcode: &Byte,
    constants: &[u64],
    heap: &Heap,
    stack_cap: usize,
) {
    let (op, slot, pool_idx) = opcode.bin_slot_imm_store_parts();
    promise!(pool_idx < constants.len());
    let packed = unsafe { *constants.get_unchecked(pool_idx) };
    let imm = packed as u32 as i32 as i64;
    let dest = (packed >> 32) as usize;
    promise!(sp + slot < stack_cap);
    let lhs = stack[sp + slot];
    let rhs = Value::from(imm);
    let result = crate::fused::eval_bin(op, lhs, rhs, heap);
    let dest_idx = sp + dest;
    promise!(dest_idx < stack_cap);
    stack[dest_idx] = result;
    let tell = stack.tell();
    if tell < dest_idx + 1 {
        stack.seek(dest_idx + 1);
    }
}

#[inline(always)]
pub(super) fn bin_slot_slot_store(
    stack: &mut Stack<Value>,
    sp: usize,
    opcode: &Byte,
    heap: &Heap,
    stack_cap: usize,
) {
    let (op, a, b, dest) = opcode.bin_slot_slot_store_parts();
    promise!(sp + a < stack_cap);
    promise!(sp + b < stack_cap);
    promise!(sp + dest < stack_cap);
    let va = stack[sp + a];
    let vb = stack[sp + b];
    let result = crate::fused::eval_bin(op, va, vb, heap);
    let dest_idx = sp + dest;
    stack[dest_idx] = result;
    let tell = stack.tell();
    if tell < dest_idx + 1 {
        stack.seek(dest_idx + 1);
    }
}

#[inline(always)]
pub(super) fn bin_slot_slot_jmp(
    stack: &Stack<Value>,
    sp: usize,
    opcode: &Byte,
    constants: &[u64],
    heap: &Heap,
    stack_cap: usize,
    want_true: bool,
) -> Option<usize> {
    let (op, a, pool_idx) = opcode.bin_slot_slot_jmpf_parts();
    promise!(pool_idx < constants.len());
    let packed = unsafe { *constants.get_unchecked(pool_idx) };
    let b = (packed as u32 & 0xFF) as usize;
    let target = (packed >> 32) as usize;
    promise!(sp + a < stack_cap);
    promise!(sp + b < stack_cap);
    let va = stack[sp + a];
    let vb = stack[sp + b];
    let taken = crate::fused::eval_cmp(op, va, vb, heap);
    if taken == want_true {
        Some(target)
    } else {
        None
    }
}

#[inline(always)]
pub(super) fn bin_slot_imm_jmp(
    stack: &Stack<Value>,
    sp: usize,
    opcode: &Byte,
    constants: &[u64],
    heap: &Heap,
    stack_cap: usize,
    want_true: bool,
) -> Option<usize> {
    let (op, slot, pool_idx) = opcode.bin_slot_imm_jmpf_parts();
    promise!(pool_idx < constants.len());
    let packed = unsafe { *constants.get_unchecked(pool_idx) };
    let imm = packed as u32 as i32 as i64;
    let target = (packed >> 32) as usize;
    promise!(sp + slot < stack_cap);
    let lhs = stack[sp + slot];
    let rhs = Value::from(imm);
    let taken = crate::fused::eval_cmp(op, lhs, rhs, heap);
    if taken == want_true {
        Some(target)
    } else {
        None
    }
}

#[inline(always)]
pub(super) fn cmp_jmp(
    stack: &mut Stack<Value>,
    opcode: &Byte,
    constants: &[u64],
    heap: &Heap,
    want_true: bool,
) -> Option<usize> {
    let (op, t) = opcode.cmp_jmpf_parts();
    let target = if opcode.cmp_jmpf_is_pool() {
        promise!(t < constants.len());
        unsafe { *constants.get_unchecked(t) as usize }
    } else {
        t
    };
    let tos = stack.tell();
    promise!(tos >= 2);
    let rhs = stack[tos - 1];
    let lhs = stack[tos - 2];
    stack.seek(tos - 2);
    let taken = crate::fused::eval_cmp(op, lhs, rhs, heap);
    if taken == want_true {
        Some(target)
    } else {
        None
    }
}

#[inline(always)]
pub(super) fn log_not_jmp(
    stack: &mut Stack<Value>,
    opcode: &Byte,
    constants: &[u64],
    want_true: bool,
) -> Option<usize> {
    let t = opcode.log_not_jmpf_target();
    let target = if opcode.log_not_jmpf_is_pool() {
        promise!(t < constants.len());
        unsafe { *constants.get_unchecked(t) as usize }
    } else {
        t
    };
    let val = stack.pop();
    if (val.as_int() == 0) == want_true {
        Some(target)
    } else {
        None
    }
}

pub(super) enum DenseFail {
    IndexOob,
    StoreNonArray,
    NoField,
    SetFieldNonInstance,
}

#[inline(always)]
pub(super) fn dense_index(
    stack: &mut Stack<Value>,
    sp: usize,
    opcode: &Byte,
    heap: &Heap,
    frames_len: usize,
    frame_pins: &mut Vec<FramePins>,
    dense_obj_addr: &mut u64,
    dense_obj: &mut Option<Object>,
    stack_cap: usize,
) -> Result<(), DenseFail> {
    let (flags, dest, arr, idx) = opcode.dense_abc_parts();
    promise!(sp + dest < stack_cap);
    promise!(sp + arr < stack_cap);
    promise!(sp + idx < stack_cap);
    let index = stack[sp + idx].as_int();
    let addr = stack[sp + arr].raw() as u64;
    let unchecked = flags & common::dense::HEAP_UNCHECKED != 0;
    let result = match resolve_dense_index_object_in(
        heap,
        frames_len,
        frame_pins,
        dense_obj_addr,
        dense_obj,
        arr as u32,
        addr,
    ) {
        Some(Object::Array(gc)) => {
            super::Machine::<8>::read_indexed(&gc.as_ref().elements, index, unchecked)
        }
        Some(Object::Tuple(gc)) => {
            super::Machine::<8>::read_indexed(&gc.as_ref().elements, index, unchecked)
        }
        _ => None,
    };
    let Some(result) = result else {
        return Err(DenseFail::IndexOob);
    };
    stack[sp + dest] = result;
    Ok(())
}

#[inline(always)]
pub(super) fn dense_store_index(
    stack: &mut Stack<Value>,
    sp: usize,
    opcode: &Byte,
    heap: &Heap,
    frames_len: usize,
    frame_pins: &mut Vec<FramePins>,
    dense_obj_addr: &mut u64,
    dense_obj: &mut Option<Object>,
    stack_cap: usize,
) -> Result<(), DenseFail> {
    let (flags, dest, arr, idx) = opcode.dense_abc_parts();
    promise!(sp + dest < stack_cap);
    promise!(sp + arr < stack_cap);
    promise!(sp + idx < stack_cap);
    let value = stack[sp + dest];
    let index = stack[sp + idx].as_int();
    let addr = stack[sp + arr].raw() as u64;
    let unchecked = flags & common::dense::HEAP_UNCHECKED != 0;
    if let Some(Object::Array(mut gc)) = resolve_dense_index_object_in(
        heap,
        frames_len,
        frame_pins,
        dense_obj_addr,
        dense_obj,
        arr as u32,
        addr,
    ) {
        let elems = &mut gc.as_mut().elements;
        if !super::Machine::<8>::write_indexed(elems, index, value, unchecked) {
            return Err(DenseFail::IndexOob);
        }
    } else {
        return Err(DenseFail::StoreNonArray);
    }
    Ok(())
}

#[inline(always)]
pub(super) fn dense_array_len(
    stack: &mut Stack<Value>,
    sp: usize,
    opcode: &Byte,
    heap: &Heap,
    stack_cap: usize,
) {
    let (dest, arr) = opcode.dense_move_parts();
    promise!(sp + dest < stack_cap);
    promise!(sp + arr < stack_cap);
    let addr = stack[sp + arr].raw() as u64;
    let len = match heap.find_object_by_addr(addr) {
        Some(Object::Array(gc)) => gc.as_ref().elements.len(),
        Some(Object::Tuple(gc)) => gc.as_ref().elements.len(),
        Some(Object::String(gc)) => gc.as_ref().data.len(),
        Some(Object::Instance(gc)) => gc
            .as_ref()
            .slot_len()
            .unwrap_or_else(|| gc.as_ref().iter_fields().count()),
        _ => 0,
    };
    stack[sp + dest] = Value::from(len as i64);
}

#[inline(always)]
pub(super) fn dense_field_load(
    stack: &mut Stack<Value>,
    sp: usize,
    opcode: &Byte,
    heap: &mut Heap,
    stack_cap: usize,
) -> Result<(), DenseFail> {
    let (flags, dest, obj, c) = opcode.dense_abc_parts();
    promise!(sp + dest < stack_cap);
    promise!(sp + obj < stack_cap);
    let addr = stack[sp + obj].raw() as u64;
    let named = flags & common::dense::FIELD_NAMED != 0;
    let result = if named {
        promise!(sp + c < stack_cap);
        let key = super::Machine::<8>::intern_key(heap, stack[sp + c]);
        match heap.find_object_by_addr(addr) {
            Some(Object::Instance(gc)) => {
                gc.as_ref().get(key).map(super::Machine::<8>::member_value)
            }
            _ => None,
        }
    } else {
        let field_index = c as usize;
        match heap.find_object_by_addr(addr) {
            Some(Object::Enum(enum_ref)) => {
                let enum_ref = enum_ref.as_ref();
                promise!(field_index < enum_ref.payload.len());
                Some(super::Machine::<8>::member_value(unsafe {
                    *enum_ref.payload.get_unchecked(field_index)
                }))
            }
            Some(Object::Instance(gc)) => {
                if let Some(n) = gc.as_ref().slot_len() {
                    promise!(field_index < n);
                    Some(super::Machine::<8>::member_value(
                        gc.as_ref()
                            .slot(field_index)
                            .unwrap_or(Member::Value(Value::default())),
                    ))
                } else {
                    Some(Value::default())
                }
            }
            _ => Some(Value::default()),
        }
    };
    let Some(result) = result else {
        return Err(DenseFail::NoField);
    };
    stack[sp + dest] = result;
    Ok(())
}

#[inline(always)]
pub(super) fn dense_field_store(
    stack: &mut Stack<Value>,
    sp: usize,
    opcode: &Byte,
    heap: &mut Heap,
    stack_cap: usize,
) -> Result<(), DenseFail> {
    let (flags, dest, obj, c) = opcode.dense_abc_parts();
    promise!(sp + dest < stack_cap);
    promise!(sp + obj < stack_cap);
    let value = stack[sp + dest];
    let addr = stack[sp + obj].raw() as u64;
    let named = flags & common::dense::FIELD_NAMED != 0;
    if let Some(Object::Instance(mut gc)) = heap.find_object_by_addr(addr) {
        if named {
            promise!(sp + c < stack_cap);
            let key = super::Machine::<8>::intern_key(heap, stack[sp + c]);
            gc.as_mut()
                .set(key, super::Machine::<8>::value_as_member(heap, value));
        } else {
            let idx = c as usize;
            promise!(gc.as_ref().slot_len().is_some_and(|n| idx < n));
            gc.as_mut()
                .set_slot(idx, super::Machine::<8>::value_as_member(heap, value));
        }
    } else {
        return Err(DenseFail::SetFieldNonInstance);
    }
    Ok(())
}

fn claim_finalizer(heap: &Heap, v: Value) -> bool {
    match heap.find_object_by_addr(v.raw() as u64) {
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

#[inline(always)]
fn do_call(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    let (arity, target) = opcode.call_parts();
    promise!(ctx.stack.tell() >= arity);
    if arity == 1
        && unlikely(!ctx.extra.finalizer_pcs.is_empty())
        && ctx.extra.finalizer_pcs.contains(&(target as u32))
    {
        promise!(ctx.stack.tell() >= 1);
        let self_val = ctx.stack[ctx.stack.tell() - 1];
        if !claim_finalizer(ctx.heap, self_val) {
            ctx.stack.pop();
            ctx.stack.push(Value::from(0i64));
            return;
        }
    }
    let callee_sp = ctx.stack.tell() - arity;
    if likely(target != 0) {
        ctx.extra.frames.rewrite_call(ctx.ip, callee_sp);
        ctx.sp = callee_sp;
        ctx.extra.frames_len = ctx.extra.frames.len();
        set_jump_target(&mut ctx.ip, target, ctx.code);
    } else {
        ctx.extra
            .frames
            .rewrite_indirect_call(ctx.ip + 1, callee_sp);
        ctx.sp = callee_sp;
        ctx.extra.frames_len = ctx.extra.frames.len();
    }
}

#[inline(always)]
fn do_tail_call(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    let (arity, target) = opcode.call_parts();
    promise!(ctx.stack.tell() >= arity);
    let callee_sp = ctx.extra.frames.top_sp();
    let src = ctx.stack.tell() - arity;
    ctx.stack.copy_slots(callee_sp, src, arity);
    ctx.stack.seek(callee_sp + arity);
    ctx.sp = callee_sp;
    set_jump_target(&mut ctx.ip, target, ctx.code);
}

fn with_coro_mut(heap: &Heap, addr: u64, f: impl FnOnce(&mut crate::ObjCoroutine)) {
    let mut current = heap.head_for_lookup();
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

#[inline(always)]
fn pop_frame_pins(frame_pins: &mut Vec<FramePins>, frames_len: usize) {
    if frame_pins.last().is_some_and(|p| p.depth == frames_len) {
        frame_pins.pop();
    }
}

#[inline(always)]
fn capture_nested(ctx: &mut HotCtx<'_, '_>, ret_val: Value) -> bool {
    if unlikely(*ctx.extra.nested_depth > 0) {
        let nested_target = ctx.extra.nested_frame_depths.last().copied().unwrap_or(0);
        if ctx.extra.frames_len == nested_target {
            *ctx.extra.nested_return = Some(ret_val);
            return true;
        }
    }
    false
}

#[inline(always)]
fn after_return_hot(ctx: &mut HotCtx<'_, '_>) {
    let (ip, sp) = ctx.extra.frames.caller_ip_sp();
    ctx.ip = ip;
    ctx.sp = sp;
    if unlikely(!ctx.extra.resume_stack.is_empty())
        && let Some(rctx) = ctx.extra.resume_stack.last()
        && ctx.extra.frames_len <= rctx.frame_depth
    {
        let coro_ptr = rctx.coro.as_ptr() as u64;
        let old_wait = {
            let mut taken = None;
            with_coro_mut(ctx.heap, coro_ptr, |coro| {
                if coro.yield_from.is_some() {
                    return;
                }
                taken = coro.io_wait.take();
                coro.state = crate::CoroState::Done;
                coro.saved_stack.clear();
                coro.saved_frames.clear();
                coro.yield_from = None;
            });
            taken
        };
        if let Some(tok) = old_wait {
            ctx.extra.io_reactor.cancel_wait(tok);
        }
        ctx.extra.resume_stack.pop();
    }
}

#[inline(always)]
fn finish_return(ctx: &mut HotCtx<'_, '_>, ret_val: Value) {
    if capture_nested(ctx, ret_val) {
        ctx.extra.execute_done = Some(false);
        return;
    }
    pop_frame_pins(ctx.extra.frame_pins, ctx.extra.frames_len);
    let return_sp = ctx.extra.frames.pop_sp();
    ctx.extra.frames_len -= 1;
    ctx.stack.seek(return_sp);
    ctx.stack.push(ret_val);
    after_return_hot(ctx);
}

#[inline(always)]
fn do_return(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    if unlikely(opcode.return_words() >= 2) {
        promise!(ctx.stack.tell() >= 2);
        let tag = ctx.stack.pop();
        let payload = ctx.stack.pop();
        if capture_nested(ctx, payload) {
            ctx.extra.execute_done = Some(false);
            return;
        }
        pop_frame_pins(ctx.extra.frame_pins, ctx.extra.frames_len);
        let return_sp = ctx.extra.frames.pop_sp();
        ctx.extra.frames_len -= 1;
        ctx.stack.seek(return_sp);
        ctx.stack.push(payload);
        ctx.stack.push(tag);
        after_return_hot(ctx);
    } else {
        let ret_val = ctx.stack.pop();
        finish_return(ctx, ret_val);
    }
}

fn apply_jump(ctx: &mut HotCtx<'_, '_>, target: Option<usize>) {
    if let Some(target) = target {
        set_jump_target(&mut ctx.ip, target, ctx.code);
    }
}

/// Consume a following `JMP` without a second dispatch (COI-387 X2).
/// Table/hotmatch only — the giant match stays byte-identical to main.
#[inline(always)]
fn apply_trailing_jmp(ctx: &mut HotCtx<'_, '_>) {
    if ctx.ip >= ctx.code.len() {
        return;
    }
    promise!(ctx.ip < ctx.code.len());
    let next = unsafe { ctx.code.get_unchecked(ctx.ip) };
    if *next.bytecode() as u8 != Instruction::JMP as u8 {
        return;
    }
    ctx.ip += 1;
    set_jump_target(&mut ctx.ip, next.operand_u32() as usize, ctx.code);
}

/// Same as [`apply_trailing_jmp`] with `unlikely` so unpaired `DenseBin`
/// (nsieve k-loop / SIMD tails) does not enlarge the fib-hot `DenseBin` arm.
#[inline(always)]
fn apply_trailing_jmp_cold(ctx: &mut HotCtx<'_, '_>) {
    if ctx.ip >= ctx.code.len() {
        return;
    }
    promise!(ctx.ip < ctx.code.len());
    let next = unsafe { ctx.code.get_unchecked(ctx.ip) };
    if unlikely(*next.bytecode() as u8 == Instruction::JMP as u8) {
        ctx.ip += 1;
        set_jump_target(&mut ctx.ip, next.operand_u32() as usize, ctx.code);
    }
}

/// Consume a leftover `DenseBin` after `DenseBin2` (COI-389 X4).
/// S2 already packed `DenseBin; DenseBin` into `DenseBin2`; odd-length
/// chains leave `DenseBin2 ; DenseBin`. Table/hotmatch only — no new
/// discriminant; giant match stays split so debugger single-step /
/// `debug_locs` still stop on the residue. `unlikely`: inner latch is
/// `DenseBin2 ; JMP` (X2), so a taken residue must not enlarge that arm.
/// Do not peek another `DenseBin2` (that would re-do S2) and do not put
/// this on the `DenseBin` arm (fib layout / S2 already ate that pair).
#[inline(always)]
fn apply_trailing_dense_bin_residue(ctx: &mut HotCtx<'_, '_>) -> bool {
    if ctx.ip >= ctx.code.len() {
        return false;
    }
    promise!(ctx.ip < ctx.code.len());
    let next = unsafe { ctx.code.get_unchecked(ctx.ip) };
    if unlikely(*next.bytecode() as u8 == Instruction::DenseBin as u8) {
        ctx.ip += 1;
        prefetch_code(ctx.code, ctx.ip);
        dense_bin(ctx.stack, ctx.sp, next, ctx.stack_cap);
        return true;
    }
    false
}

/// Consume a following `DenseBin` / `DenseBin2` without a second dispatch
/// (COI-380 S3). Table/hotmatch only — giant match stays two-dispatch; no
/// new discriminant. After S2 the header shape is `DenseCast ; DenseBin2`.
/// Returns whether a bin was consumed so S5 can also peek a latch `JMP`.
/// After a peeked `DenseBin2`, also take X4 residue `DenseBin`.
#[inline(always)]
fn apply_trailing_dense_bin(ctx: &mut HotCtx<'_, '_>) -> bool {
    if ctx.ip >= ctx.code.len() {
        return false;
    }
    promise!(ctx.ip < ctx.code.len());
    let next = unsafe { ctx.code.get_unchecked(ctx.ip) };
    let bc = *next.bytecode() as u8;
    if bc == Instruction::DenseBin as u8 {
        ctx.ip += 1;
        prefetch_code(ctx.code, ctx.ip);
        dense_bin(ctx.stack, ctx.sp, next, ctx.stack_cap);
        return true;
    }
    if bc == Instruction::DenseBin2 as u8 {
        ctx.ip += 1;
        prefetch_code(ctx.code, ctx.ip);
        let tail = take_code_word(ctx);
        dense_bin2(ctx.stack, ctx.sp, next, &tail, ctx.stack_cap);
        let _ = apply_trailing_dense_bin_residue(ctx);
        return true;
    }
    false
}

/// nsieve k-loop: `DenseStoreIndex ; DenseBin ; JMP` (COI-382 S5). Peek the
/// IV bump and latch without a new discriminant. Giant match stays split.
#[inline(always)]
fn apply_trailing_dense_bin_jmp(ctx: &mut HotCtx<'_, '_>) {
    if apply_trailing_dense_bin(ctx) {
        apply_trailing_jmp(ctx);
    }
}

fn cold(_ctx: &mut HotCtx<'_, '_>, _opcode: Byte) {}

#[inline(never)]
fn op_dense_bin(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    dense_bin(ctx.stack, ctx.sp, &opcode, ctx.stack_cap);
    apply_trailing_jmp_cold(ctx);
}

#[inline(never)]
fn op_dense_bin2(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    let tail = take_code_word(ctx);
    dense_bin2(ctx.stack, ctx.sp, &opcode, &tail, ctx.stack_cap);
    let _ = apply_trailing_dense_bin_residue(ctx);
    apply_trailing_jmp(ctx);
}

#[inline(never)]
fn op_dense_bin_jmpf(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    dense_bin(ctx.stack, ctx.sp, &opcode, ctx.stack_cap);
    let tail = take_code_word(ctx);
    let target = dense_bin_jmp_tail(
        ctx.stack,
        ctx.sp,
        &tail,
        ctx.constants,
        ctx.heap,
        ctx.stack_cap,
    );
    apply_jump(ctx, target);
}

#[inline(never)]
fn op_dense_index_jmpf(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    if dense_index(
        ctx.stack,
        ctx.sp,
        &opcode,
        ctx.heap,
        ctx.extra.frames_len,
        ctx.extra.frame_pins,
        ctx.extra.dense_obj_addr,
        ctx.extra.dense_obj,
        ctx.stack_cap,
    )
    .is_err()
    {
        ctx.panic_msg = Some("index out of bounds");
        return;
    }
    let tail = take_code_word(ctx);
    let target = dense_bin_jmp_tail(
        ctx.stack,
        ctx.sp,
        &tail,
        ctx.constants,
        ctx.heap,
        ctx.stack_cap,
    );
    apply_jump(ctx, target);
}

#[inline(never)]
fn op_dense_cmp(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    dense_cmp(ctx.stack, ctx.sp, &opcode, ctx.stack_cap);
}

#[inline(never)]
fn op_dense_const(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    dense_const(ctx.stack, ctx.sp, &opcode, ctx.constants, ctx.stack_cap);
}

#[inline(never)]
fn op_dense_move(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    dense_move(ctx.stack, ctx.sp, &opcode, ctx.stack_cap);
}

#[inline(never)]
fn op_dense_cast(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    dense_cast(ctx.stack, ctx.sp, &opcode, ctx.stack_cap);
    let _ = apply_trailing_dense_bin(ctx);
}

#[inline(never)]
fn op_dense_unary(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    dense_unary(ctx.stack, ctx.sp, &opcode, ctx.stack_cap);
}

#[inline(never)]
fn op_load(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    load(ctx.stack, ctx.sp, &opcode, ctx.stack_cap);
}

#[inline(never)]
fn op_store(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    store(ctx.stack, ctx.sp, &opcode, ctx.stack_cap);
}

#[inline(never)]
fn op_seek(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    seek(ctx.stack, ctx.sp, &opcode, ctx.stack_cap);
}

#[inline(never)]
fn op_jmp(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    set_jump_target(&mut ctx.ip, opcode.operand_u32() as usize, ctx.code);
}

#[inline(never)]
fn op_jmpf(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    if !ctx.stack.pop().as_bool() {
        set_jump_target(&mut ctx.ip, opcode.operand_u32() as usize, ctx.code);
    }
}

#[inline(never)]
fn op_jmpt(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    if ctx.stack.pop().as_bool() {
        set_jump_target(&mut ctx.ip, opcode.operand_u32() as usize, ctx.code);
    }
}

#[inline(never)]
fn op_bin_slot_slot_jmpf(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    apply_jump(
        ctx,
        bin_slot_slot_jmp(
            ctx.stack,
            ctx.sp,
            &opcode,
            ctx.constants,
            ctx.heap,
            ctx.stack_cap,
            false,
        ),
    );
}

#[inline(never)]
fn op_bin_slot_slot_jmpt(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    apply_jump(
        ctx,
        bin_slot_slot_jmp(
            ctx.stack,
            ctx.sp,
            &opcode,
            ctx.constants,
            ctx.heap,
            ctx.stack_cap,
            true,
        ),
    );
}

#[inline(never)]
fn op_bin_slot_imm_jmpf(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    apply_jump(
        ctx,
        bin_slot_imm_jmp(
            ctx.stack,
            ctx.sp,
            &opcode,
            ctx.constants,
            ctx.heap,
            ctx.stack_cap,
            false,
        ),
    );
}

#[inline(never)]
fn op_bin_slot_imm_jmpt(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    apply_jump(
        ctx,
        bin_slot_imm_jmp(
            ctx.stack,
            ctx.sp,
            &opcode,
            ctx.constants,
            ctx.heap,
            ctx.stack_cap,
            true,
        ),
    );
}

#[inline(never)]
fn op_cmp_jmpf(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    let target = cmp_jmp(ctx.stack, &opcode, ctx.constants, ctx.heap, false);
    apply_jump(ctx, target);
}

#[inline(never)]
fn op_cmp_jmpt(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    let target = cmp_jmp(ctx.stack, &opcode, ctx.constants, ctx.heap, true);
    apply_jump(ctx, target);
}

#[inline(never)]
fn op_log_not_jmpf(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    let target = log_not_jmp(ctx.stack, &opcode, ctx.constants, false);
    apply_jump(ctx, target);
}

#[inline(never)]
fn op_log_not_jmpt(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    let target = log_not_jmp(ctx.stack, &opcode, ctx.constants, true);
    apply_jump(ctx, target);
}

#[inline(never)]
fn op_bin_slot_imm(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    bin_slot_imm(ctx.stack, ctx.sp, &opcode, ctx.heap, ctx.stack_cap);
}

#[inline(never)]
fn op_bin_slot_imm_store(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    bin_slot_imm_store(
        ctx.stack,
        ctx.sp,
        &opcode,
        ctx.constants,
        ctx.heap,
        ctx.stack_cap,
    );
}

#[inline(never)]
fn op_bin_slot_slot_store(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    bin_slot_slot_store(ctx.stack, ctx.sp, &opcode, ctx.heap, ctx.stack_cap);
}

#[inline(never)]
fn op_dense_index(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    if dense_index(
        ctx.stack,
        ctx.sp,
        &opcode,
        ctx.heap,
        ctx.extra.frames_len,
        ctx.extra.frame_pins,
        ctx.extra.dense_obj_addr,
        ctx.extra.dense_obj,
        ctx.stack_cap,
    )
    .is_err()
    {
        ctx.panic_msg = Some("index out of bounds");
    }
}

#[inline(never)]
fn op_dense_store_index(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    match dense_store_index(
        ctx.stack,
        ctx.sp,
        &opcode,
        ctx.heap,
        ctx.extra.frames_len,
        ctx.extra.frame_pins,
        ctx.extra.dense_obj_addr,
        ctx.extra.dense_obj,
        ctx.stack_cap,
    ) {
        Ok(()) => apply_trailing_dense_bin_jmp(ctx),
        Err(DenseFail::IndexOob) => ctx.panic_msg = Some("index out of bounds"),
        Err(_) => ctx.panic_msg = Some("StoreIndex on non-array"),
    }
}

#[inline(never)]
fn op_dense_array_len(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    dense_array_len(ctx.stack, ctx.sp, &opcode, ctx.heap, ctx.stack_cap);
}

#[inline(never)]
fn op_dense_field_load(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    if dense_field_load(ctx.stack, ctx.sp, &opcode, ctx.heap, ctx.stack_cap).is_err() {
        ctx.panic_msg = Some("no such field");
    }
}

#[inline(never)]
fn op_dense_field_store(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    if dense_field_store(ctx.stack, ctx.sp, &opcode, ctx.heap, ctx.stack_cap).is_err() {
        ctx.panic_msg = Some("SetField on non-instance");
    }
}

#[inline(never)]
fn op_call(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    do_call(ctx, opcode);
}

#[inline(never)]
fn op_tail_call(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    do_tail_call(ctx, opcode);
}

#[inline(never)]
fn op_return(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    do_return(ctx, opcode);
}

#[inline(never)]
fn op_const_return_imm(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    let ret_val = Value::from(opcode.operand_u32() as i32 as i64 as u64);
    finish_return(ctx, ret_val);
}

#[inline(never)]
fn op_load_return_slot(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    let slot = opcode.operand_u32() as usize;
    promise!(ctx.sp + slot < ctx.stack_cap);
    let ret_val = ctx.stack[ctx.sp + slot];
    finish_return(ctx, ret_val);
}

#[inline(never)]
fn op_bin_return(ctx: &mut HotCtx<'_, '_>, opcode: Byte) {
    let tos = ctx.stack.tell();
    promise!(tos >= 2);
    let rhs = ctx.stack[tos - 1];
    let lhs = ctx.stack[tos - 2];
    let ret_val = crate::fused::eval_bin(opcode.bin_return_op(), lhs, rhs, ctx.heap);
    finish_return(ctx, ret_val);
}

fn build_table() -> [Handler; 256] {
    refresh_opt_call_return_hot();
    let mut t = [cold as Handler; 256];
    t[Instruction::DenseBin as usize] = op_dense_bin;
    t[Instruction::DenseBin2 as usize] = op_dense_bin2;
    t[Instruction::DenseBinJmpf as usize] = op_dense_bin_jmpf;
    t[Instruction::DenseIndexJmpf as usize] = op_dense_index_jmpf;
    t[Instruction::DenseCmp as usize] = op_dense_cmp;
    t[Instruction::DenseConst as usize] = op_dense_const;
    t[Instruction::DenseMove as usize] = op_dense_move;
    t[Instruction::DenseCast as usize] = op_dense_cast;
    t[Instruction::DenseUnary as usize] = op_dense_unary;
    t[Instruction::DenseIndex as usize] = op_dense_index;
    t[Instruction::DenseStoreIndex as usize] = op_dense_store_index;
    t[Instruction::DenseArrayLen as usize] = op_dense_array_len;
    t[Instruction::DenseFieldLoad as usize] = op_dense_field_load;
    t[Instruction::DenseFieldStore as usize] = op_dense_field_store;
    t[Instruction::JMP as usize] = op_jmp;
    t[Instruction::JMPF as usize] = op_jmpf;
    t[Instruction::JMPT as usize] = op_jmpt;
    t[Instruction::BinSlotSlotJmpf as usize] = op_bin_slot_slot_jmpf;
    t[Instruction::BinSlotSlotJmpt as usize] = op_bin_slot_slot_jmpt;
    t[Instruction::CmpJmpf as usize] = op_cmp_jmpf;
    t[Instruction::CmpJmpt as usize] = op_cmp_jmpt;
    t[Instruction::LogNotJmpf as usize] = op_log_not_jmpf;
    t[Instruction::LogNotJmpt as usize] = op_log_not_jmpt;
    t[Instruction::BinSlotSlotStore as usize] = op_bin_slot_slot_store;
    // Imm-slot fuses share the fib/tak kernel with CALL/RETURN. Threading
    // them without both call and return bounces out of the trampoline.
    if call_is_hot() && return_is_hot() {
        t[Instruction::BinSlotImm as usize] = op_bin_slot_imm;
        t[Instruction::BinSlotImmStore as usize] = op_bin_slot_imm_store;
        t[Instruction::BinSlotImmJmpf as usize] = op_bin_slot_imm_jmpf;
        t[Instruction::BinSlotImmJmpt as usize] = op_bin_slot_imm_jmpt;
        t[Instruction::CALL as usize] = op_call;
        t[Instruction::TailCall as usize] = op_tail_call;
        t[Instruction::RETURN as usize] = op_return;
        t[Instruction::ConstReturnImm as usize] = op_const_return_imm;
        t[Instruction::LoadReturnSlot as usize] = op_load_return_slot;
        t[Instruction::BinReturn as usize] = op_bin_return;
    }
    t
}

fn table() -> &'static [Handler; 256] {
    static CELL: OnceLock<[Handler; 256]> = OnceLock::new();
    CELL.get_or_init(build_table)
}

#[inline(always)]
fn copy_byte(code: &[Byte], ip: usize) -> Byte {
    promise!(ip < code.len());
    let src = unsafe { code.get_unchecked(ip) };
    Byte::new(*src.bytecode()).with_operand_u32(src.operand_u32())
}

#[inline(always)]
fn exec_hot(ctx: &mut HotCtx<'_, '_>, bc: Instruction, opcode: Byte) {
    match bc {
        Instruction::DenseBin => {
            dense_bin(ctx.stack, ctx.sp, &opcode, ctx.stack_cap);
            apply_trailing_jmp_cold(ctx);
        }
        Instruction::DenseBin2 => {
            let tail = take_code_word(ctx);
            dense_bin2(ctx.stack, ctx.sp, &opcode, &tail, ctx.stack_cap);
            let _ = apply_trailing_dense_bin_residue(ctx);
            apply_trailing_jmp(ctx);
        }
        Instruction::DenseBinJmpf => {
            dense_bin(ctx.stack, ctx.sp, &opcode, ctx.stack_cap);
            let tail = take_code_word(ctx);
            let target = dense_bin_jmp_tail(
                ctx.stack,
                ctx.sp,
                &tail,
                ctx.constants,
                ctx.heap,
                ctx.stack_cap,
            );
            apply_jump(ctx, target);
        }
        Instruction::DenseIndexJmpf => {
            if dense_index(
                ctx.stack,
                ctx.sp,
                &opcode,
                ctx.heap,
                ctx.extra.frames_len,
                ctx.extra.frame_pins,
                ctx.extra.dense_obj_addr,
                ctx.extra.dense_obj,
                ctx.stack_cap,
            )
            .is_err()
            {
                ctx.panic_msg = Some("index out of bounds");
                return;
            }
            let tail = take_code_word(ctx);
            let target = dense_bin_jmp_tail(
                ctx.stack,
                ctx.sp,
                &tail,
                ctx.constants,
                ctx.heap,
                ctx.stack_cap,
            );
            apply_jump(ctx, target);
        }
        Instruction::DenseCmp => dense_cmp(ctx.stack, ctx.sp, &opcode, ctx.stack_cap),
        Instruction::DenseConst => {
            dense_const(ctx.stack, ctx.sp, &opcode, ctx.constants, ctx.stack_cap)
        }
        Instruction::DenseMove => dense_move(ctx.stack, ctx.sp, &opcode, ctx.stack_cap),
        Instruction::DenseCast => {
            dense_cast(ctx.stack, ctx.sp, &opcode, ctx.stack_cap);
            let _ = apply_trailing_dense_bin(ctx);
        }
        Instruction::DenseUnary => dense_unary(ctx.stack, ctx.sp, &opcode, ctx.stack_cap),
        Instruction::LOAD => load(ctx.stack, ctx.sp, &opcode, ctx.stack_cap),
        Instruction::STORE => store(ctx.stack, ctx.sp, &opcode, ctx.stack_cap),
        Instruction::Seek => seek(ctx.stack, ctx.sp, &opcode, ctx.stack_cap),
        Instruction::JMP => {
            set_jump_target(&mut ctx.ip, opcode.operand_u32() as usize, ctx.code);
        }
        Instruction::JMPF => {
            if !ctx.stack.pop().as_bool() {
                set_jump_target(&mut ctx.ip, opcode.operand_u32() as usize, ctx.code);
            }
        }
        Instruction::JMPT => {
            if ctx.stack.pop().as_bool() {
                set_jump_target(&mut ctx.ip, opcode.operand_u32() as usize, ctx.code);
            }
        }
        Instruction::BinSlotSlotJmpf => apply_jump(
            ctx,
            bin_slot_slot_jmp(
                ctx.stack,
                ctx.sp,
                &opcode,
                ctx.constants,
                ctx.heap,
                ctx.stack_cap,
                false,
            ),
        ),
        Instruction::BinSlotSlotJmpt => apply_jump(
            ctx,
            bin_slot_slot_jmp(
                ctx.stack,
                ctx.sp,
                &opcode,
                ctx.constants,
                ctx.heap,
                ctx.stack_cap,
                true,
            ),
        ),
        Instruction::BinSlotImmJmpf => apply_jump(
            ctx,
            bin_slot_imm_jmp(
                ctx.stack,
                ctx.sp,
                &opcode,
                ctx.constants,
                ctx.heap,
                ctx.stack_cap,
                false,
            ),
        ),
        Instruction::BinSlotImmJmpt => apply_jump(
            ctx,
            bin_slot_imm_jmp(
                ctx.stack,
                ctx.sp,
                &opcode,
                ctx.constants,
                ctx.heap,
                ctx.stack_cap,
                true,
            ),
        ),
        Instruction::CmpJmpf => {
            let target = cmp_jmp(ctx.stack, &opcode, ctx.constants, ctx.heap, false);
            apply_jump(ctx, target);
        }
        Instruction::CmpJmpt => {
            let target = cmp_jmp(ctx.stack, &opcode, ctx.constants, ctx.heap, true);
            apply_jump(ctx, target);
        }
        Instruction::LogNotJmpf => {
            let target = log_not_jmp(ctx.stack, &opcode, ctx.constants, false);
            apply_jump(ctx, target);
        }
        Instruction::LogNotJmpt => {
            let target = log_not_jmp(ctx.stack, &opcode, ctx.constants, true);
            apply_jump(ctx, target);
        }
        Instruction::BinSlotImm => {
            bin_slot_imm(ctx.stack, ctx.sp, &opcode, ctx.heap, ctx.stack_cap)
        }
        Instruction::BinSlotImmStore => bin_slot_imm_store(
            ctx.stack,
            ctx.sp,
            &opcode,
            ctx.constants,
            ctx.heap,
            ctx.stack_cap,
        ),
        Instruction::BinSlotSlotStore => {
            bin_slot_slot_store(ctx.stack, ctx.sp, &opcode, ctx.heap, ctx.stack_cap)
        }
        Instruction::DenseIndex => {
            if dense_index(
                ctx.stack,
                ctx.sp,
                &opcode,
                ctx.heap,
                ctx.extra.frames_len,
                ctx.extra.frame_pins,
                ctx.extra.dense_obj_addr,
                ctx.extra.dense_obj,
                ctx.stack_cap,
            )
            .is_err()
            {
                ctx.panic_msg = Some("index out of bounds");
            }
        }
        Instruction::DenseStoreIndex => match dense_store_index(
            ctx.stack,
            ctx.sp,
            &opcode,
            ctx.heap,
            ctx.extra.frames_len,
            ctx.extra.frame_pins,
            ctx.extra.dense_obj_addr,
            ctx.extra.dense_obj,
            ctx.stack_cap,
        ) {
            Ok(()) => apply_trailing_dense_bin_jmp(ctx),
            Err(DenseFail::IndexOob) => ctx.panic_msg = Some("index out of bounds"),
            Err(_) => ctx.panic_msg = Some("StoreIndex on non-array"),
        },
        Instruction::DenseArrayLen => {
            dense_array_len(ctx.stack, ctx.sp, &opcode, ctx.heap, ctx.stack_cap)
        }
        Instruction::DenseFieldLoad => {
            if dense_field_load(ctx.stack, ctx.sp, &opcode, ctx.heap, ctx.stack_cap).is_err() {
                ctx.panic_msg = Some("no such field");
            }
        }
        Instruction::DenseFieldStore => {
            if dense_field_store(ctx.stack, ctx.sp, &opcode, ctx.heap, ctx.stack_cap).is_err() {
                ctx.panic_msg = Some("SetField on non-instance");
            }
        }
        Instruction::CALL if call_is_hot() => do_call(ctx, opcode),
        Instruction::TailCall if call_is_hot() => do_tail_call(ctx, opcode),
        Instruction::RETURN if return_is_hot() => do_return(ctx, opcode),
        Instruction::ConstReturnImm if return_is_hot() => {
            let ret_val = Value::from(opcode.operand_u32() as i32 as i64 as u64);
            finish_return(ctx, ret_val);
        }
        Instruction::LoadReturnSlot if return_is_hot() => {
            let slot = opcode.operand_u32() as usize;
            promise!(ctx.sp + slot < ctx.stack_cap);
            let ret_val = ctx.stack[ctx.sp + slot];
            finish_return(ctx, ret_val);
        }
        Instruction::BinReturn if return_is_hot() => {
            let tos = ctx.stack.tell();
            promise!(tos >= 2);
            let rhs = ctx.stack[tos - 1];
            let lhs = ctx.stack[tos - 2];
            let ret_val = crate::fused::eval_bin(opcode.bin_return_op(), lhs, rhs, ctx.heap);
            finish_return(ctx, ret_val);
        }
        _ => {}
    }
}

#[inline(never)]
fn table_loop(ctx: &mut HotCtx<'_, '_>) {
    let handlers = table();
    let code_len = ctx.code.len();
    loop {
        if unlikely(ctx.ip >= code_len) {
            return;
        }
        promise!(ctx.ip < code_len);
        let opcode = copy_byte(ctx.code, ctx.ip);
        let disc = *opcode.bytecode() as u8 as usize;
        let h = handlers[disc];
        if unlikely(core::ptr::fn_addr_eq(h, cold as Handler)) {
            return;
        }
        super::note_dispatch_at(ctx.ip, ctx.stack, ctx.sp);
        ctx.ip += 1;
        prefetch_code(ctx.code, ctx.ip);
        h(ctx, opcode);
        if unlikely(ctx.panic_msg.is_some() || ctx.extra.execute_done.is_some()) {
            return;
        }
    }
}

#[inline(never)]
fn hotmatch_loop(ctx: &mut HotCtx<'_, '_>) {
    let code_len = ctx.code.len();
    loop {
        if unlikely(ctx.ip >= code_len) {
            return;
        }
        promise!(ctx.ip < code_len);
        let opcode = copy_byte(ctx.code, ctx.ip);
        let bc = *opcode.bytecode();
        if unlikely(!is_hot(bc)) {
            return;
        }
        super::note_dispatch_at(ctx.ip, ctx.stack, ctx.sp);
        ctx.ip += 1;
        prefetch_code(ctx.code, ctx.ip);
        exec_hot(ctx, bc, opcode);
        if unlikely(ctx.panic_msg.is_some() || ctx.extra.execute_done.is_some()) {
            return;
        }
    }
}

pub(super) enum HotStop {
    Panic(&'static str),
    Done(bool),
}

/// Consume a streak of hot ops at `*ip`. Leaves `*ip` on the first cold op
/// (or `code.len()`).
#[inline(never)]
pub(super) fn run_hot_streak<const S: usize>(
    stack: &mut Stack<Value>,
    sp: &mut usize,
    ip: &mut usize,
    code: &[Byte],
    constants: &[u64],
    heap: &mut Heap,
    frames: &mut ArrayVec<Frame, S>,
    frame_pins: &mut Vec<FramePins>,
    dense_obj_addr: &mut u64,
    dense_obj: &mut Option<Object>,
    finalizer_pcs: &std::collections::HashSet<u32, crate::AddrHashBuilder>,
    nested_depth: &mut u32,
    nested_frame_depths: &mut Vec<usize>,
    nested_return: &mut Option<Value>,
    resume_stack: &mut Vec<super::ResumeCtx>,
    io_reactor: &std::sync::Arc<crate::io_reactor::IoReactor>,
    stack_cap: usize,
    mode: Mode,
) -> Option<HotStop> {
    let frames_len = frames.len();
    let mut extra = HotExtra {
        frames: frames as &mut dyn CallFrames,
        frames_len,
        frame_pins,
        dense_obj_addr,
        dense_obj,
        finalizer_pcs,
        nested_depth,
        nested_frame_depths,
        nested_return,
        resume_stack,
        io_reactor,
        execute_done: None,
    };
    let mut ctx = HotCtx {
        stack,
        sp: *sp,
        ip: *ip,
        code,
        constants,
        heap,
        stack_cap,
        panic_msg: None,
        extra: &mut extra,
    };
    match mode {
        Mode::Table => table_loop(&mut ctx),
        Mode::HotMatch => hotmatch_loop(&mut ctx),
        Mode::Match => {}
    }
    *ip = ctx.ip;
    *sp = ctx.sp;
    if let Some(msg) = ctx.panic_msg {
        Some(HotStop::Panic(msg))
    } else {
        ctx.extra.execute_done.map(HotStop::Done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mode_aliases() {
        assert_eq!(parse_mode("0"), Mode::Match);
        assert_eq!(parse_mode("match"), Mode::Match);
        assert_eq!(parse_mode("1"), Mode::Table);
        assert_eq!(parse_mode("table"), Mode::Table);
        assert_eq!(parse_mode("2"), Mode::HotMatch);
        assert_eq!(parse_mode("hotmatch"), Mode::HotMatch);
    }

    #[test]
    fn hot_subset_and_table_slots() {
        let _ = table();
        assert!(is_hot(Instruction::DenseBin));
        assert!(is_hot(Instruction::DenseBin2));
        assert!(is_hot(Instruction::DenseBinJmpf));
        assert!(is_hot(Instruction::DenseIndexJmpf));
        assert!(is_hot(Instruction::BinSlotSlotJmpf));
        assert!(is_hot(Instruction::DenseIndex));
        assert!(is_hot(Instruction::DenseCast));
        assert!(!is_hot(Instruction::LOAD));
        assert!(!is_hot(Instruction::Seek));
        assert!(!is_hot(Instruction::HALT));
        assert_eq!(
            is_hot(Instruction::CALL),
            call_is_hot() && return_is_hot()
        );
        assert_eq!(
            is_hot(Instruction::RETURN),
            call_is_hot() && return_is_hot()
        );
        assert!(!is_hot(Instruction::BinSlotImmJmpf) || (call_is_hot() && return_is_hot()));
        assert!(!is_hot(Instruction::BinSlotImm) || (call_is_hot() && return_is_hot()));
        assert!(!is_hot(Instruction::BinSlotImmJmpt) || (call_is_hot() && return_is_hot()));
        assert!(!is_hot(Instruction::BinReturn) || (call_is_hot() && return_is_hot()));
        assert!(!is_hot(Instruction::ConstReturnImm) || (call_is_hot() && return_is_hot()));
        // COI-388: alloc+return stays on the giant match (not ALWAYS_HOT).
        assert!(!is_hot(Instruction::MakeEnumReturn));
        let t = table();
        assert!(!core::ptr::fn_addr_eq(
            t[Instruction::DenseBin as usize],
            cold as Handler
        ));
        assert_eq!(
            !core::ptr::fn_addr_eq(t[Instruction::RETURN as usize], cold as Handler),
            return_is_hot()
        );
        assert_eq!(
            !core::ptr::fn_addr_eq(t[Instruction::CALL as usize], cold as Handler),
            call_is_hot()
        );
    }
}
