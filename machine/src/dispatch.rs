//! G0 (COI-373): outlined hot-op dispatch vs the giant `Machine::execute` match.
//!
//! Stable Rust has no guaranteed tail calls (`become` is nightly), so this
//! spike cannot use musttail. A 256-entry fn-pointer trampoline is the portable
//! stand-in; `hotmatch` is a compact-match control in the same outlined function.
//!
//! Select at process start with `COIL_THREADED_DISPATCH`:
//! - `0` / `match` — existing giant match (A/B baseline)
//! - `1` / `table` / unset — fn-pointer trampoline (spike default)
//! - `2` / `hotmatch` — compact match over the same hot subset
//!
//! Debugger-attached runs stay on the giant match so per-op stops still fire.

use std::cell::Cell;
use std::sync::OnceLock;

use common::{
    ArchivedByte as Byte, ArchivedInstruction as Instruction, Value, promise, unlikely,
};

use super::{prefetch_code, set_jump_target};
use crate::{Heap, Stack};

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
    *CACHED.get_or_init(|| match std::env::var("COIL_THREADED_DISPATCH") {
        Ok(v) => parse_mode(&v),
        Err(_) => Mode::Table,
    })
}

#[cfg(test)]
pub(super) fn override_mode(mode: Option<Mode>) {
    MODE_OVERRIDE.with(|c| c.set(mode));
}

#[inline(always)]
pub(super) fn is_hot(bc: Instruction) -> bool {
    matches!(
        bc,
        Instruction::DenseBin
            | Instruction::DenseCmp
            | Instruction::DenseConst
            | Instruction::DenseMove
            | Instruction::DenseCast
            | Instruction::JMP
            | Instruction::JMPF
            | Instruction::JMPT
            | Instruction::BinSlotSlotJmpf
            | Instruction::BinSlotSlotJmpt
    )
}

struct HotCtx<'a> {
    stack: &'a mut Stack<Value>,
    sp: usize,
    ip: usize,
    code: &'a [Byte],
    constants: &'a [u64],
    heap: &'a Heap,
    stack_cap: usize,
}

type Handler = fn(&mut HotCtx<'_>, Byte);

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

fn cold(_ctx: &mut HotCtx<'_>, _opcode: Byte) {}

#[inline(never)]
fn op_dense_bin(ctx: &mut HotCtx<'_>, opcode: Byte) {
    dense_bin(ctx.stack, ctx.sp, &opcode, ctx.stack_cap);
}

#[inline(never)]
fn op_dense_cmp(ctx: &mut HotCtx<'_>, opcode: Byte) {
    dense_cmp(ctx.stack, ctx.sp, &opcode, ctx.stack_cap);
}

#[inline(never)]
fn op_dense_const(ctx: &mut HotCtx<'_>, opcode: Byte) {
    dense_const(ctx.stack, ctx.sp, &opcode, ctx.constants, ctx.stack_cap);
}

#[inline(never)]
fn op_dense_move(ctx: &mut HotCtx<'_>, opcode: Byte) {
    dense_move(ctx.stack, ctx.sp, &opcode, ctx.stack_cap);
}

#[inline(never)]
fn op_dense_cast(ctx: &mut HotCtx<'_>, opcode: Byte) {
    dense_cast(ctx.stack, ctx.sp, &opcode, ctx.stack_cap);
}

#[inline(never)]
fn op_jmp(ctx: &mut HotCtx<'_>, opcode: Byte) {
    set_jump_target(&mut ctx.ip, opcode.operand_u32() as usize, ctx.code);
}

#[inline(never)]
fn op_jmpf(ctx: &mut HotCtx<'_>, opcode: Byte) {
    if !ctx.stack.pop().as_bool() {
        set_jump_target(&mut ctx.ip, opcode.operand_u32() as usize, ctx.code);
    }
}

#[inline(never)]
fn op_jmpt(ctx: &mut HotCtx<'_>, opcode: Byte) {
    if ctx.stack.pop().as_bool() {
        set_jump_target(&mut ctx.ip, opcode.operand_u32() as usize, ctx.code);
    }
}

#[inline(never)]
fn op_bin_slot_slot_jmpf(ctx: &mut HotCtx<'_>, opcode: Byte) {
    if let Some(target) = bin_slot_slot_jmp(
        ctx.stack,
        ctx.sp,
        &opcode,
        ctx.constants,
        ctx.heap,
        ctx.stack_cap,
        false,
    ) {
        set_jump_target(&mut ctx.ip, target, ctx.code);
    }
}

#[inline(never)]
fn op_bin_slot_slot_jmpt(ctx: &mut HotCtx<'_>, opcode: Byte) {
    if let Some(target) = bin_slot_slot_jmp(
        ctx.stack,
        ctx.sp,
        &opcode,
        ctx.constants,
        ctx.heap,
        ctx.stack_cap,
        true,
    ) {
        set_jump_target(&mut ctx.ip, target, ctx.code);
    }
}

fn build_table() -> [Handler; 256] {
    let mut t = [cold as Handler; 256];
    t[Instruction::DenseBin as usize] = op_dense_bin;
    t[Instruction::DenseCmp as usize] = op_dense_cmp;
    t[Instruction::DenseConst as usize] = op_dense_const;
    t[Instruction::DenseMove as usize] = op_dense_move;
    t[Instruction::DenseCast as usize] = op_dense_cast;
    t[Instruction::JMP as usize] = op_jmp;
    t[Instruction::JMPF as usize] = op_jmpf;
    t[Instruction::JMPT as usize] = op_jmpt;
    t[Instruction::BinSlotSlotJmpf as usize] = op_bin_slot_slot_jmpf;
    t[Instruction::BinSlotSlotJmpt as usize] = op_bin_slot_slot_jmpt;
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
fn exec_hot(ctx: &mut HotCtx<'_>, bc: Instruction, opcode: Byte) {
    match bc {
        Instruction::DenseBin => dense_bin(ctx.stack, ctx.sp, &opcode, ctx.stack_cap),
        Instruction::DenseCmp => dense_cmp(ctx.stack, ctx.sp, &opcode, ctx.stack_cap),
        Instruction::DenseConst => {
            dense_const(ctx.stack, ctx.sp, &opcode, ctx.constants, ctx.stack_cap)
        }
        Instruction::DenseMove => dense_move(ctx.stack, ctx.sp, &opcode, ctx.stack_cap),
        Instruction::DenseCast => dense_cast(ctx.stack, ctx.sp, &opcode, ctx.stack_cap),
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
        Instruction::BinSlotSlotJmpf => {
            if let Some(target) = bin_slot_slot_jmp(
                ctx.stack,
                ctx.sp,
                &opcode,
                ctx.constants,
                ctx.heap,
                ctx.stack_cap,
                false,
            ) {
                set_jump_target(&mut ctx.ip, target, ctx.code);
            }
        }
        Instruction::BinSlotSlotJmpt => {
            if let Some(target) = bin_slot_slot_jmp(
                ctx.stack,
                ctx.sp,
                &opcode,
                ctx.constants,
                ctx.heap,
                ctx.stack_cap,
                true,
            ) {
                set_jump_target(&mut ctx.ip, target, ctx.code);
            }
        }
        _ => {}
    }
}

#[inline(never)]
fn table_loop(ctx: &mut HotCtx<'_>) {
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
    }
}

#[inline(never)]
fn hotmatch_loop(ctx: &mut HotCtx<'_>) {
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
    }
}

/// Consume a streak of hot ops at `*ip`. Leaves `*ip` on the first cold op
/// (or `code.len()`).
#[inline(never)]
pub(super) fn run_hot_streak(
    stack: &mut Stack<Value>,
    sp: usize,
    ip: &mut usize,
    code: &[Byte],
    constants: &[u64],
    heap: &Heap,
    stack_cap: usize,
    mode: Mode,
) {
    let mut ctx = HotCtx {
        stack,
        sp,
        ip: *ip,
        code,
        constants,
        heap,
        stack_cap,
    };
    match mode {
        Mode::Table => table_loop(&mut ctx),
        Mode::HotMatch => hotmatch_loop(&mut ctx),
        Mode::Match => {}
    }
    *ip = ctx.ip;
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
        assert!(is_hot(Instruction::DenseBin));
        assert!(is_hot(Instruction::BinSlotSlotJmpf));
        assert!(!is_hot(Instruction::CALL));
        assert!(!is_hot(Instruction::HALT));
        let t = table();
        assert!(!core::ptr::fn_addr_eq(
            t[Instruction::DenseBin as usize],
            cold as Handler
        ));
        assert!(core::ptr::fn_addr_eq(
            t[Instruction::CALL as usize],
            cold as Handler
        ));
    }
}
