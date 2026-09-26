//! Dense hot-streak execution for `Machine::execute`.
//!
//! The giant match in `vm.rs` stays the only dispatch loop. After an
//! always-hot opcode (dense kernel ops, jumps, fused compare-branches) it
//! hands the streak to `execute_dense`, a compact match that runs until the
//! first op outside `ALWAYS_HOT`, so a dense loop does not bounce back to the
//! giant match per opcode. CALL / RETURN / packed LOAD / STORE stay on the
//! giant match. FORMAT / HostInvoke / `rest` live in `.text.unlikely` so they
//! are not in the hot working set.
//!
//! A 256-entry fn-pointer table and a compact "hotmatch" loop were measured as
//! A/B alternatives (COI-373..376, `COIL_THREADED_DISPATCH`) and removed: they
//! won ~8% on `fib`, lost ~20% on `tak`, and were flat elsewhere.

use common::{
    ArchivedByte as Byte, ArchivedInstruction as Instruction, ArrayVec, Value, promise,
    unlikely,
};

use super::{FramePins, prefetch_code, resolve_dense_index_object_in, set_jump_target};
use crate::{Frame, Heap, Member, Object, Stack};

struct HotExtra<'a> {
    frames_len: usize,
    frame_pins: &'a mut Vec<FramePins>,
    dense_obj_addr: &'a mut u64,
    dense_obj: &'a mut Option<Object>,
}

/// Outcome of a remaining (non-kernel) op executed via [`super::Machine::exec_rest`].
pub(super) enum RestFlow {
    Continue,
    Done(bool),
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

#[inline(always)]
pub(super) fn is_always_hot_disc(disc: u8) -> bool {
    let i = disc as usize;
    (ALWAYS_HOT[i >> 6] >> (i & 63)) & 1 != 0
}

#[inline(always)]
fn is_always_hot(bc: Instruction) -> bool {
    is_always_hot_disc(bc as u8)
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
pub(super) fn dense_bin_jmp_tail<H: crate::fused::HeapView>(
    stack: &mut Stack<Value>,
    sp: usize,
    tail: &Byte,
    constants: &[u64],
    heap: &H,
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
pub(super) fn bin_slot_imm<H: crate::fused::HeapView>(
    stack: &mut Stack<Value>,
    sp: usize,
    opcode: &Byte,
    heap: &H,
    stack_cap: usize,
) {
    let (op, slot, imm) = opcode.bin_slot_imm_parts();
    promise!(sp + slot < stack_cap);
    let lhs = stack[sp + slot];
    let rhs = Value::from(imm);
    stack.push(crate::fused::eval_bin(op, lhs, rhs, heap));
}

#[inline(always)]
pub(super) fn bin_slot_imm_store<H: crate::fused::HeapView>(
    stack: &mut Stack<Value>,
    sp: usize,
    opcode: &Byte,
    constants: &[u64],
    heap: &H,
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
pub(super) fn bin_slot_slot_store<H: crate::fused::HeapView>(
    stack: &mut Stack<Value>,
    sp: usize,
    opcode: &Byte,
    heap: &H,
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
pub(super) fn bin_slot_slot_jmp<H: crate::fused::HeapView>(
    stack: &Stack<Value>,
    sp: usize,
    opcode: &Byte,
    constants: &[u64],
    heap: &H,
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
pub(super) fn bin_slot_imm_jmp<H: crate::fused::HeapView>(
    stack: &Stack<Value>,
    sp: usize,
    opcode: &Byte,
    constants: &[u64],
    heap: &H,
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
pub(super) fn cmp_jmp<H: crate::fused::HeapView>(
    stack: &mut Stack<Value>,
    opcode: &Byte,
    constants: &[u64],
    heap: &H,
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

pub(super) struct DenseIndexArgs<'a> {
    pub stack: &'a mut Stack<Value>,
    pub sp: usize,
    pub opcode: &'a Byte,
    pub heap: &'a Heap,
    pub frames_len: usize,
    pub frame_pins: &'a mut Vec<FramePins>,
    pub dense_obj_addr: &'a mut u64,
    pub dense_obj: &'a mut Option<Object>,
    pub stack_cap: usize,
}

#[inline(always)]
pub(super) fn dense_index(args: DenseIndexArgs<'_>) -> Result<(), DenseFail> {
    let DenseIndexArgs {
        stack,
        sp,
        opcode,
        heap,
        frames_len,
        frame_pins,
        dense_obj_addr,
        dense_obj,
        stack_cap,
    } = args;
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

pub(super) struct DenseStoreIndexArgs<'a> {
    pub stack: &'a mut Stack<Value>,
    pub sp: usize,
    pub opcode: &'a Byte,
    pub heap: &'a Heap,
    pub frames_len: usize,
    pub frame_pins: &'a mut Vec<FramePins>,
    pub dense_obj_addr: &'a mut u64,
    pub dense_obj: &'a mut Option<Object>,
    pub stack_cap: usize,
}

#[inline(always)]
pub(super) fn dense_store_index(args: DenseStoreIndexArgs<'_>) -> Result<(), DenseFail> {
    let DenseStoreIndexArgs {
        stack,
        sp,
        opcode,
        heap,
        frames_len,
        frame_pins,
        dense_obj_addr,
        dense_obj,
        stack_cap,
    } = args;
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
        let field_index = c;
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
            let idx = c;
            promise!(gc.as_ref().slot_len().is_some_and(|n| idx < n));
            gc.as_mut()
                .set_slot(idx, super::Machine::<8>::value_as_member(heap, value));
        }
    } else {
        return Err(DenseFail::SetFieldNonInstance);
    }
    Ok(())
}

/// Direct unary call whose callee starts with `slot0 ? imm; jump ConstReturnImm`.
///
/// Same compare the callee would run. When the branch is taken, the call
/// returns that constant and does not push a frame. Any other shape returns
/// `None` and the caller performs a normal `CALL`.
#[inline(always)]
pub(super) fn unary_const_base_return<H: crate::fused::HeapView>(
    code: &[Byte],
    constants: &[u64],
    target: usize,
    arg: Value,
    heap: &H,
) -> Option<Value> {
    if target >= code.len() {
        return None;
    }
    let entry = unsafe { code.get_unchecked(target) };
    let want_true = match *entry.bytecode() {
        Instruction::BinSlotImmJmpt => true,
        Instruction::BinSlotImmJmpf => false,
        _ => return None,
    };
    let (cmp_op, slot, pool_idx) = entry.bin_slot_imm_jmpf_parts();
    if slot != 0 || pool_idx >= constants.len() {
        return None;
    }
    let packed = unsafe { *constants.get_unchecked(pool_idx) };
    let imm = packed as u32 as i32 as i64;
    let dest = (packed >> 32) as usize;
    if dest >= code.len() {
        return None;
    }
    let ret_op = unsafe { code.get_unchecked(dest) };
    if !matches!(*ret_op.bytecode(), Instruction::ConstReturnImm) {
        return None;
    }
    let taken = crate::fused::eval_cmp(cmp_op, arg, Value::from(imm), heap);
    if taken != want_true {
        return None;
    }
    Some(Value::from(ret_op.operand_u32() as i32 as i64 as u64))
}

fn apply_jump(ctx: &mut HotCtx<'_, '_>, target: Option<usize>) {
    if let Some(target) = target {
        set_jump_target(&mut ctx.ip, target, ctx.code);
    }
}

/// Consume a following `JMP` without a second dispatch (COI-387 X2).
/// Dense streak only — the giant match does not peek.
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
/// chains leave `DenseBin2 ; DenseBin`. Dense streak only — no new
/// discriminant; giant match stays split so debugger single-step /
/// `debug_locs` still stop on the residue. `unlikely` keeps unpaired
/// `DenseBin2 ; JMP` (X2, no leftover) from enlarging the taken latch;
/// mandelbrot still has leftover+JMP on the inner iter. Do not peek
/// another `DenseBin2` and do not put this on the `DenseBin` arm.
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
/// (COI-380 S3). Dense streak only — giant match stays two-dispatch; no
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

#[inline(always)]
fn copy_byte(code: &[Byte], ip: usize) -> Byte {
    promise!(ip < code.len());
    // `ArchivedByte` is plain data but not `Copy`. One word load; rebuilding
    // the opcode and operand reloads the same instruction on every dense dispatch.
    unsafe { core::ptr::read(code.as_ptr().add(ip)) }
}

/// ALWAYS_HOT kernel only — no CALL/LOAD/imm, so `execute_dense` does not
/// pull FORMAT / HostInvoke / return text into the same match (COI-376).
#[inline(always)]
fn exec_dense(ctx: &mut HotCtx<'_, '_>, bc: Instruction, opcode: Byte) {
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
            if dense_index(DenseIndexArgs {
                stack: ctx.stack,
                sp: ctx.sp,
                opcode: &opcode,
                heap: ctx.heap,
                frames_len: ctx.extra.frames_len,
                frame_pins: ctx.extra.frame_pins,
                dense_obj_addr: ctx.extra.dense_obj_addr,
                dense_obj: ctx.extra.dense_obj,
                stack_cap: ctx.stack_cap,
            })
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
        Instruction::BinSlotSlotStore => {
            bin_slot_slot_store(ctx.stack, ctx.sp, &opcode, ctx.heap, ctx.stack_cap)
        }
        Instruction::DenseIndex => {
            if dense_index(DenseIndexArgs {
                stack: ctx.stack,
                sp: ctx.sp,
                opcode: &opcode,
                heap: ctx.heap,
                frames_len: ctx.extra.frames_len,
                frame_pins: ctx.extra.frame_pins,
                dense_obj_addr: ctx.extra.dense_obj_addr,
                dense_obj: ctx.extra.dense_obj,
                stack_cap: ctx.stack_cap,
            })
            .is_err()
            {
                ctx.panic_msg = Some("index out of bounds");
            }
        }
        Instruction::DenseStoreIndex => match dense_store_index(DenseStoreIndexArgs {
            stack: ctx.stack,
            sp: ctx.sp,
            opcode: &opcode,
            heap: ctx.heap,
            frames_len: ctx.extra.frames_len,
            frame_pins: ctx.extra.frame_pins,
            dense_obj_addr: ctx.extra.dense_obj_addr,
            dense_obj: ctx.extra.dense_obj,
            stack_cap: ctx.stack_cap,
        }) {
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
        Instruction::DenseFieldStore
            if dense_field_store(ctx.stack, ctx.sp, &opcode, ctx.heap, ctx.stack_cap).is_err() => {
                ctx.panic_msg = Some("SetField on non-instance");
            }
        _ => {}
    }
}

/// Dedicated dense-kernel entry: compact match, no handler table (COI-376).
/// Stops on the first non-`ALWAYS_HOT` op (peek, no new discriminant).
#[inline(never)]
#[cfg_attr(target_os = "linux", unsafe(link_section = ".text.hot"))]
fn execute_dense(ctx: &mut HotCtx<'_, '_>) {
    let code_len = ctx.code.len();
    loop {
        if unlikely(ctx.ip >= code_len) {
            return;
        }
        promise!(ctx.ip < code_len);
        let opcode = copy_byte(ctx.code, ctx.ip);
        let bc = *opcode.bytecode();
        if unlikely(!is_always_hot(bc)) {
            return;
        }
        super::note_dispatch_at(ctx.ip, ctx.stack, ctx.sp);
        ctx.ip += 1;
        prefetch_code(ctx.code, ctx.ip);
        exec_dense(ctx, bc, opcode);
        // Panic is the only stop.
        if unlikely(ctx.panic_msg.is_some()) {
            return;
        }
    }
}

pub(super) struct ConsumeAlwaysHotStreakArgs<'a, const S: usize> {
    pub stack: &'a mut Stack<Value>,
    pub sp: &'a mut usize,
    pub ip: &'a mut usize,
    pub code: &'a [Byte],
    pub constants: &'a [u64],
    pub heap: &'a mut Heap,
    pub frames: &'a mut ArrayVec<Frame, S>,
    pub frame_pins: &'a mut Vec<FramePins>,
    pub dense_obj_addr: &'a mut u64,
    pub dense_obj: &'a mut Option<Object>,
    pub stack_cap: usize,
}

/// After the giant match handles one always-hot opcode, keep going through
/// `execute_dense` while the following words are still always-hot.
///
/// `*ip` already points at the next instruction, which the caller has
/// checked with [`is_always_hot_disc`]. Fib never calls this.
/// Mandelbrot enters once and stays in the dense loop across the back edge.
#[inline(never)]
pub(super) fn consume_always_hot_streak<const S: usize>(
    args: ConsumeAlwaysHotStreakArgs<'_, S>,
) -> Option<&'static str> {
    let ConsumeAlwaysHotStreakArgs {
        stack,
        sp,
        ip,
        code,
        constants,
        heap,
        frames,
        frame_pins,
        dense_obj_addr,
        dense_obj,
        stack_cap,
    } = args;
    if *ip >= code.len() {
        return None;
    }
    let disc = unsafe { *code.get_unchecked(*ip).bytecode() } as u8;
    if !is_always_hot_disc(disc) {
        return None;
    }
    let frames_len = frames.len();
    let mut extra = HotExtra {
        frames_len,
        frame_pins,
        dense_obj_addr,
        dense_obj,
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
    execute_dense(&mut ctx);
    *ip = ctx.ip;
    *sp = ctx.sp;
    ctx.panic_msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_hot_subset() {
        for op in [
            Instruction::DenseBin,
            Instruction::DenseBin2,
            Instruction::DenseBinJmpf,
            Instruction::DenseIndexJmpf,
            Instruction::BinSlotSlotJmpf,
            Instruction::DenseIndex,
            Instruction::DenseCast,
            Instruction::JMP,
        ] {
            assert!(is_always_hot(op), "{}", op as u8);
        }
        // CALL/RETURN/LOAD/STORE and alloc+return stay on the giant match.
        for op in [
            Instruction::LOAD,
            Instruction::Seek,
            Instruction::HALT,
            Instruction::ADD,
            Instruction::FORMAT,
            Instruction::STRINGIFY,
            Instruction::HostInvoke,
            Instruction::CALL,
            Instruction::RETURN,
            Instruction::BinSlotImm,
            Instruction::BinSlotImmJmpt,
            Instruction::BinReturn,
            Instruction::ConstReturnImm,
            Instruction::MakeEnumReturn,
        ] {
            assert!(!is_always_hot(op), "{}", op as u8);
        }
    }
}
