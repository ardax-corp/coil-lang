//! Try to replace a numeric IL body with dense MIR bytecode, or lift
//! an eligible leftover body through MIR→LIR (I8).

use common::Instruction;

use crate::il::{IlOp, Label};

use std::collections::HashMap;

use super::abi::{DenseAbi, DenseCallMap};
use super::deopt::DraftDeoptMap;
use super::emit::emit_dense;
use super::gc::refuses_alloc;

/// C3 sidecar from a dense / LIR replace (named lets + deopt maps).
#[derive(Clone, Debug, Default)]
pub struct BodySidecar {
    pub debug_slot_remap: HashMap<u32, u32>,
    pub deopt: Option<DraftDeoptMap>,
    /// Allocating dense body: its registers may sit past the cursor, so its
    /// frame needs a decoded extent (see `precise_frames`).
    pub needs_frame_extent: bool,
    /// GC map of a LIR body built from MIR lowered from HIR (no stack IL
    /// lift to draw it from).
    pub stack_map: Option<super::stackmap::DraftFrameMap>,
}

type RefusalSlot = std::thread::LocalKey<std::cell::RefCell<Option<String>>>;

/// `(function, "dense" | "lir", MIR text)` captured for `coil dissect --mir`.
pub type CapturedMir = Vec<(String, &'static str, String)>;

thread_local! {
    /// `coil dissect --mir`: `(function, form, MIR text)` of each body that
    /// reached emission. `None` when not capturing.
    static MIR_CAPTURE: std::cell::RefCell<Option<CapturedMir>> =
        const { std::cell::RefCell::new(None) };
    static DENSE_REFUSAL: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    static LIR_REFUSAL: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Start recording the final MIR of each specialized body (dissect).
pub fn start_mir_capture() {
    MIR_CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
}

/// Stop recording and return `(function, "dense" | "lir", MIR text)`.
pub fn take_mir_capture() -> CapturedMir {
    MIR_CAPTURE.with(|c| c.borrow_mut().take().unwrap_or_default())
}

fn capture_mir(name: &str, form: &'static str, func: &crate::mir::MirFunc) {
    MIR_CAPTURE.with(|c| {
        if let Some(list) = c.borrow_mut().as_mut() {
            list.push((name.to_string(), form, func.to_string()));
        }
    });
}

/// Why the last [`try_specialize_body_side`] returned `None` (keep-rate census).
pub fn take_dense_refusal() -> Option<String> {
    DENSE_REFUSAL.with(|c| c.borrow_mut().take())
}

/// Why the last [`try_lower_abi_body_side`] returned `None` (keep-rate census).
pub fn take_lir_refusal() -> Option<String> {
    LIR_REFUSAL.with(|c| c.borrow_mut().take())
}

/// Record a coarse refusal key (digits folded) in `slot` and return `None`.
fn note_refusal<T>(slot: &'static RefusalSlot, why: impl std::fmt::Display) -> Option<T> {
    let key: String = why
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .take(60)
        .collect();
    slot.with(|c| *c.borrow_mut() = Some(key));
    None
}

fn refuse_dense<T>(why: impl std::fmt::Display) -> Option<T> {
    note_refusal(&DENSE_REFUSAL, why)
}

fn refuse<T>(why: impl std::fmt::Display) -> Option<T> {
    note_refusal(&LIR_REFUSAL, why)
}

/// `official_entry` is `IlFunc.meta.entry` (CALL target). New labels must
/// not reuse that id or concat lands calls on a loop header.
///
/// Dense-specialize one body from `from_hir` (MIR lowered from HIR):
/// dense IL plus its ABI when it beats `ops` (fuse-IL).
///
/// CSE → LICM → CSE → InstCombine (incl. P11 float peeps) → DestProp → SR → CSE → GVN/PRE,
/// then saxpy-reduce HostInvoke (P12) or dense emit (W4 allowlisted
/// HostInvoke edges box → call → unbox; COI-291 dense→dense `CALL` when
/// `calls` lists the callee).
#[allow(clippy::too_many_arguments)]
pub fn try_specialize_body_side(
    ops: &[IlOp],
    name: &str,
    pool: &mut Vec<u64>,
    calls: &DenseCallMap,
    official_entry: Option<Label>,
    from_hir: Option<&crate::mir::MirFunc>,
    side: &mut BodySidecar,
) -> Option<(Vec<IlOp>, DenseAbi)> {
    let Some(func) = from_hir else {
        return refuse_dense("no MIR from HIR");
    };
    let mut direct_side = BodySidecar::default();
    let mut direct_pool = pool.clone();
    let out = specialize_side(ops, name, &mut direct_pool, calls, official_entry, func, &mut direct_side)?;
    *pool = direct_pool;
    *side = direct_side;
    crate::il::opt::note_hir_mir_kept();
    Some(out)
}

#[allow(clippy::too_many_arguments)]
fn specialize_side(
    ops: &[IlOp],
    name: &str,
    pool: &mut Vec<u64>,
    calls: &DenseCallMap,
    official_entry: Option<Label>,
    from_hir: &crate::mir::MirFunc,
    side: &mut BodySidecar,
) -> Option<(Vec<IlOp>, DenseAbi)> {
    // Nested / multi-header numeric loops are eligible (flagship mandelbrot).
    // Infer requires float +/−/×/÷, counted i64 +/−/×/÷/%, or i32. A
    // back-edge or a straight-line numeric body may lift; keep/refuse is
    // the cost gate vs fuse-IL (A3). S3/S3b: one-word CALL (dense map or
    // open), I6 HostInvoke including Q9 R2 `from_bytes` / `to_bytes`, heap
    // index / ArrayLen / StoreIndex (dense residuals after V*). FORMAT /
    // STRING / PRINT / STRINGIFY stay off dense (Q9 R1 is MIR→LIR; R3
    // maps FORMAT / STRINGIFY). Q6
    // counted `for` is i64 + index.
    // Q8: niche / two-slot match may dense when the reconstruct beats
    // fuse-IL. D3: boxed JumpIfMatch / multi-payload Unpack may dense
    // (stack JIM + per-index MatchPayload; cost gate). Alloc / InitTyped without
    // S2b maps take dense with a frame extent (minor 22). S2d: mapped *preheader*
    // Make* + index loop may take dense. A2: Index / Make* / ArrayLen /
    // StoreIndex emit dense-native. D2: heap LoadField / GetField /
    // SetField emit DenseField*; Object Alloc is DenseMakeObject.
    // CALL / HostInvoke box at the ABI
    // edge via DensePush. S2l: try dense so SROA / LICM can delete work;
    // keep residual in-loop Make* only when reconstruct is dense-native
    // and cost ≤ fuse-IL. Post-loop-only `return [x]` stays fuse-IL
    // (COI-87 invert+fuse). Debugger-attached / -Og still enter (B8).
    // S2k: last-arm writes must survive; Seek size is a cost, not a cap.
    // B7 / C1: sibling / mutual TailCall and self multi-word CALL/RETURN
    // may lift (stack-arg protocol, reserved callee entry labels). N>2
    // still refuses at infer/lower (`MAX_MODELED_RET_WORDS`).
    let select_cfg = has_sroa_select_cfg(ops);
    let has_alloc = ops.iter().any(refuses_alloc);
    let inloop_alloc = super::infer::has_alloc_inside_loop(ops);
    if has_alloc && super::infer::has_post_loop_alloc_return(ops) {
        return refuse_dense("post-loop alloc return");
    }
    // Registers may sit past the cursor at a safepoint, and generic host
    // results are heap words typed `i64` that S2b maps miss: cover the frame.
    side.needs_frame_extent = has_alloc;
    if !has_numeric_arith(from_hir) {
        return refuse_dense("no numeric arith");
    }
    if !calls_agree(from_hir, calls) {
        return refuse_dense("call ABI");
    }
    let mut func = from_hir.clone();
    // Stack-IL CSE refuses DIVF; number it on SSA before dense emit.
    crate::mir::cse(&mut func);
    crate::mir::licm(&mut func);
    crate::mir::cse(&mut func);
    crate::mir::instcombine(&mut func);
    crate::mir::destprop(&mut func);
    crate::mir::strength_reduce(&mut func);
    crate::mir::cse(&mut func);
    crate::mir::gvn(&mut func);
    // S2f: reuse the mutated array after StoreIndex (drop rematerialized Alloc).
    crate::mir::sroa(&mut func);
    super::deopt::fill_deopt_maps(&mut func);
    paint_index_dest_from_uses(&mut func);
    let stores_ssa = func
        .blocks
        .iter()
        .flat_map(|b| b.insts.iter())
        .filter(|i| matches!(i, crate::mir::MirInst::StoreIndex { .. }))
        .count();
    if count_store_index(ops) > 0 && stores_ssa == 0 {
        return refuse_dense("store index dropped in SSA");
    }
    let Some(abi) = DenseAbi::from_func_and_live_ins(&func, ops, &HashMap::new()) else {
        return refuse_dense("dense abi");
    };
    let entry = official_entry.or_else(|| {
        ops.iter().find_map(|op| match op {
            IlOp::Label(l) | IlOp::JoinLabel(l) => Some(*l),
            _ => None,
        })
    });
    let label_hi = crate::il::opt::max_code_label(ops)
        .max(official_entry.map(|Label(id)| id).unwrap_or(0));
    let (remap, deopt) = super::emit::dense_sidecars(&func, entry);
    side.debug_slot_remap = remap;
    side.deopt = Some(deopt);
    if let Some(packed) = super::pack::try_axpy_pack(&func, entry, pool) {
        return Some((packed, abi));
    }
    if let Some(vecd) = super::vectorize::try_vectorize(&func, entry, pool, label_hi) {
        return Some((vecd, abi));
    }
    capture_mir(name, "dense", &func);
    let out = match emit_dense(&func, entry, pool, has_alloc) {
        Ok(o) => o,
        Err(e) => return refuse_dense(format!("emit: {e}")),
    };
    // Heap writes have no SSA users; refuse if reconstruct dropped one.
    if count_store_index(&out) < count_store_index(ops) {
        return refuse_dense("store index dropped in emit");
    }
    // Const-fold must not erase every Index (for-in / invert+fuse leftover).
    if count_index(ops) > 0 && count_index(&out) == 0 {
        return refuse_dense("index erased");
    }
    if select_cfg && !select_reconstruct_ok(ops, &out) {
        return refuse_dense("select reconstruct");
    }
    // In-loop Make*: keep dense only when heap ops are native (S2l / A2).
    if inloop_alloc
        && super::infer::has_alloc_inside_loop(&out)
        && residual_heap_box(&out)
    {
        return refuse_dense("boxed in-loop alloc");
    }
    // Straight-line / leftover reconstruct: never denser-but-slower.
    // Loops amortize prologue Seek; they skip this static compare unless
    // the body is a select diamond or still has in-loop Make*.
    // Native in-loop Make* / ArrayPush amortize prologue Seek (A2 / B6).
    // Boxed leftover reconstruct is already refused above.
    let loop_tax = super::infer::has_back_edge(ops) && !select_cfg;
    // Q7 self-CALL is eligible; keep/refuse is still this cost compare.
    // B2 convoys CALL on the stack so tight fib/tak can win without
    // skipping the gate (the old Seek + STORE reconstruct was ~2× fib).
    if !loop_tax && emit_cost(&out) > emit_cost(ops) {
        return refuse_dense("dense cost gate");
    }
    // Two-slot CALL/RETURN must not grow a boxed reconstruct (C1).
    if count_make_enum(&out) > count_make_enum(ops) {
        return refuse_dense("MakeEnum growth");
    }
    Some((out, abi))
}

/// Every `CALL` in `func` uses its callee's dense ABI, when the callee has one.
fn calls_agree(func: &crate::mir::MirFunc, calls: &DenseCallMap) -> bool {
    use crate::mir::MirInst;
    func.blocks.iter().flat_map(|b| b.insts.iter()).all(|i| match i {
        MirInst::Call { dest, dest_hi, target, args } => calls.get(&target.0).is_none_or(|abi| {
            abi.ret_hi == dest_hi.map(|h| func.ty(h))
                && abi.ret == func.ty(*dest)
                && abi.params.len() == args.len()
                && abi.params.iter().zip(args).all(|(&p, &a)| p == func.ty(a))
        }),
        _ => true,
    })
}

/// The arithmetic dense pays for: float `+ - * /` or int `+ - * / %`.
fn has_numeric_arith(func: &crate::mir::MirFunc) -> bool {
    use crate::mir::{MirBinOp, MirInst};
    func.blocks.iter().flat_map(|b| b.insts.iter()).any(|i| {
        matches!(
            i,
            MirInst::Bin { op: MirBinOp::Add | MirBinOp::Sub | MirBinOp::Mul | MirBinOp::Div, ty, .. } if ty.is_float() || ty.is_int()
        ) || matches!(i, MirInst::Bin { op: MirBinOp::Rem, ty, .. } if ty.is_int())
    })
}

/// Index / open CALL dests default to i64 when the next IL is StorePop.
/// Repaint from float uses so DenseBin / RETURN keep the Value bits.
fn paint_index_dest_from_uses(func: &mut crate::mir::func::MirFunc) {
    use crate::mir::inst::{MirInst, ValueId};
    let mut paint = Vec::new();
    for b in &func.blocks {
        for inst in &b.insts {
            match inst {
                MirInst::Index { dest, .. } | MirInst::Call { dest, .. } => {
                    paint.push(*dest);
                }
                _ => {}
            }
        }
    }
    if paint.is_empty() {
        return;
    }
    let mut ty_of = vec![None; func.types.len()];
    for b in &func.blocks {
        for inst in &b.insts {
            match inst {
                MirInst::Bin { ty, lhs, rhs, .. } if ty.is_float() => {
                    ty_of[lhs.index()] = Some(*ty);
                    ty_of[rhs.index()] = Some(*ty);
                }
                MirInst::StoreIndex { value, .. } => {
                    let vt = func.ty(*value);
                    if vt.is_float() {
                        ty_of[value.index()] = Some(vt);
                    }
                }
                _ => {}
            }
        }
    }
    for ValueId(id) in paint {
        if let Some(ty) = ty_of.get(id as usize).copied().flatten() {
            func.types[id as usize] = ty;
        }
    }
}

fn count_make_enum(ops: &[IlOp]) -> usize {
    ops.iter()
        .filter(|op| match op {
            IlOp::MakeEnum { .. } => true,
            IlOp::Byte { byte, .. } => {
                matches!(
                    *byte.bytecode(),
                    Instruction::MakeEnum
                        | Instruction::MakeEnumReturn
                        | Instruction::MakeEnumK
                        | Instruction::MakeEnumReturnK
                )
            }
            _ => false,
        })
        .count()
}

fn count_store_index(ops: &[IlOp]) -> usize {
    ops.iter()
        .filter(|op| match op {
            IlOp::StoreIndexPin { .. } | IlOp::StoreIndexPinUnchecked { .. } => true,
            IlOp::Byte { byte, .. } => matches!(
                *byte.bytecode(),
                Instruction::StoreIndex
                    | Instruction::StoreIndexUnchecked
                    | Instruction::DenseStoreIndex
            ),
            _ => false,
        })
        .count()
}

fn count_index(ops: &[IlOp]) -> usize {
    ops.iter()
        .filter(|op| match op {
            IlOp::Index { .. }
            | IlOp::IndexUnchecked { .. }
            | IlOp::IndexPin { .. }
            | IlOp::IndexPinUnchecked { .. } => true,
            IlOp::Byte { byte, .. } => matches!(
                *byte.bytecode(),
                Instruction::Index | Instruction::IndexUnchecked | Instruction::DenseIndex
                    | Instruction::DenseIndexJmpf
            ),
            _ => false,
        })
        .count()
}

fn is_stack_heap_op(inst: Instruction) -> bool {
    matches!(
        inst,
        Instruction::Index
            | Instruction::IndexUnchecked
            | Instruction::StoreIndex
            | Instruction::StoreIndexUnchecked
            | Instruction::ArrayLen
            | Instruction::MakeArray
            | Instruction::MakeArrayK
            | Instruction::MakeTuple
            | Instruction::MakeTupleK
            | Instruction::MakeEnum
            | Instruction::MakeEnumK
            | Instruction::MakeEnumReturn
            | Instruction::MakeEnumReturnK
            | Instruction::InitTyped
    )
}

/// LOAD / StorePop around stack Index / Make* — the A2 tax we refuse to ship.
fn residual_heap_box(ops: &[IlOp]) -> bool {
    ops.iter().any(|op| match op {
        IlOp::Index { .. }
        | IlOp::IndexUnchecked { .. }
        | IlOp::MakeArray { .. }
        | IlOp::MakeTuple { .. }
        | IlOp::MakeEnum { .. } => true,
        IlOp::Byte { byte, .. } => is_stack_heap_op(*byte.bytecode()),
        _ => false,
    })
}

/// Reconstruct size vs fuse-IL. Weights `Seek` and LOAD/STORE, not opcode
/// novelty — packed `Dense*` must not grow this (COI-385 S8).
fn emit_cost(ops: &[IlOp]) -> usize {
    ops.iter()
        .filter(|op| !matches!(op, IlOp::Label(_) | IlOp::JoinLabel(_)))
        .map(|op| match op {
            IlOp::StorePop { .. } | IlOp::Load { .. } => 2,
            IlOp::Byte { byte, .. } => match *byte.bytecode() {
                Instruction::Seek => 2 + (byte.operand_u32() as usize) / 16,
                Instruction::LOAD | Instruction::StorePop => 2,
                _ => 1,
            },
            _ => 1,
        })
        .sum()
}

/// MIR→LIR for a body dense specialize misses (I8): `from_hir` (MIR
/// lowered from HIR) emitted as stack IL with `emit_lir` keeping
/// single-use values on the stack. Keep/refuse is the LIR cost gate in
/// `IlModule`, not a feature floor.
pub fn try_lower_abi_body_side(
    ops: &[IlOp],
    name: &str,
    pool: &mut Vec<u64>,
    from_hir: Option<&crate::mir::MirFunc>,
    side: &mut BodySidecar,
) -> Option<Vec<IlOp>> {
    // I8: any inferable unfused body, not only two-slot / match / field accidents.
    // S2c: allocating leftovers need a real S2b draft; else fuse-IL.
    // S2d: mapped in-loop / preheader Make* may reconstruct; post-loop-only
    // `return [x]` stays fuse-IL so invert+fuse (COI-87) remains.
    let has_alloc = ops.iter().any(refuses_alloc);
    if has_alloc && super::infer::has_post_loop_alloc_return(ops) {
        return refuse("post-loop alloc return");
    }
    // MIR lowered from HIR maps its own allocations (`lir_tail` builds the
    // body's draft from it).
    let Some(direct) = from_hir else {
        return refuse("no MIR from HIR");
    };
    if has_alloc && !super::stackmap::mir_maps_ok(direct) {
        return refuse("no MIR maps");
    }
    if let Some(wall) = super::entry::lir_refuse_direct(ops, has_alloc) {
        return refuse(format!("lir wall {wall:?}"));
    }
    let mut func = direct.clone();
    if has_alloc {
        crate::mir::sroa(&mut func);
    } else {
        crate::mir::cse(&mut func);
    }
    lir_tail(ops, name, pool, has_alloc, func, side)
}

/// Emit the LIR tier's stack IL from `func`.
#[allow(clippy::too_many_arguments)]
fn lir_tail(ops: &[IlOp], name: &str, pool: &mut Vec<u64>, has_alloc: bool, mut func: crate::mir::MirFunc, side: &mut BodySidecar) -> Option<Vec<IlOp>> {
    super::deopt::fill_deopt_maps(&mut func);
    let entry = ops.iter().find_map(|op| match op {
        IlOp::Label(l) | IlOp::JoinLabel(l) => Some(*l),
        _ => None,
    });
    // Every label leading the body binds the entry; a self call may name any.
    let aliases: Vec<Label> = ops
        .iter()
        .map_while(|op| match op {
            IlOp::Label(l) | IlOp::JoinLabel(l) => Some(*l),
            _ => None,
        })
        .filter(|&l| Some(l) != entry)
        .collect();
    let (remap, deopt, mut stack_map) = super::emit_lir::lir_sidecars(&func);
    side.debug_slot_remap = remap;
    side.deopt = Some(deopt);
    // SROA may have taken every allocation out; then there is nothing to map.
    if func.has_gc_edge() {
        let Some(map) = stack_map.take() else {
            return refuse("no MIR stack map");
        };
        let mut map = map;
        map.name = name.to_string();
        side.stack_map = Some(map);
    }
    capture_mir(name, "lir", &func);
    let out = match super::emit_lir::emit_lir_with_aliases(&func, entry, &aliases, pool, has_alloc) {
        Ok(o) => o,
        Err(e) => return refuse(format!("emit: {e}")),
    };
    if has_sroa_select_cfg(ops) && !select_reconstruct_ok(ops, &out) {
        return refuse("select reconstruct");
    }
    if count_make_enum(&out) > count_make_enum(ops) {
        return refuse("MakeEnum growth");
    }
    Some(out)
}

/// Last-arm writes must remain as `StorePop` (LIR) or `DenseMove` (dense
/// phi copies). Seek frame size is measured in [`emit_cost`], not capped.
fn select_reconstruct_ok(src: &[IlOp], out: &[IlOp]) -> bool {
    last_arm_writes(out) >= last_arm_writes(src)
}

/// Writes before the first branch are not arm writes: a prologue store that
/// falls into the loop head is not one, however the head is laid out.
fn last_arm_writes(ops: &[IlOp]) -> usize {
    let first = ops.iter().position(is_cond_branch).unwrap_or(ops.len());
    let mut n = 0usize;
    for (i, op) in ops.iter().enumerate().skip(first) {
        if !is_select_write(op) {
            continue;
        }
        if is_last_arm_write(ops, i) {
            n += 1;
        }
    }
    n
}

fn is_select_write(op: &IlOp) -> bool {
    matches!(op, IlOp::StorePop { .. })
        || matches!(
            op,
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::STORE | Instruction::StorePop | Instruction::DenseMove
                )
        )
}

/// A last arm falls into a join: write, then only labels until a join bind
/// (no intervening jump).
fn is_last_arm_write(ops: &[IlOp], write_i: usize) -> bool {
    let mut saw_join = false;
    for op in &ops[write_i + 1..] {
        match op {
            IlOp::Label(_) | IlOp::JoinLabel(_) => saw_join = true,
            IlOp::Jump { .. } | IlOp::Return { .. } | IlOp::Halt { .. } => return false,
            _ if saw_join => return true,
            _ => {}
        }
    }
    saw_join
}

/// S2f computed-index slot-select: several EQ/JMPF arms into one join.
fn has_sroa_select_cfg(ops: &[IlOp]) -> bool {
    use crate::il::IlJumpKind;
    let mut n = 0usize;
    for (i, op) in ops.iter().enumerate() {
        match op {
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue,
                ..
            } if eq_immediately_before(ops, i) => n += 1,
            IlOp::Byte { byte, .. } if fused_eq_jmp(byte) => n += 1,
            _ => {}
        }
    }
    n >= 2
}

fn is_cond_branch(op: &IlOp) -> bool {
    use crate::il::IlJumpKind;
    match op {
        IlOp::Jump { kind, .. } => *kind != IlJumpKind::Unconditional,
        IlOp::Byte { byte, .. } => matches!(
            *byte.bytecode(),
            Instruction::BinSlotImmJmpf
                | Instruction::BinSlotImmJmpt
                | Instruction::BinSlotSlotConstJmpf
                | Instruction::BinSlotSlotConstJmpt
                | Instruction::BinSlotSlotJmpf
                | Instruction::BinSlotSlotJmpt
                | Instruction::CmpJmpf
                | Instruction::CmpJmpt
                | Instruction::DenseBinJmpf
                | Instruction::DenseIndexJmpf
                | Instruction::LogNotJmpf
                | Instruction::LogNotJmpt
        ),
        _ => false,
    }
}

fn eq_immediately_before(ops: &[IlOp], jump_i: usize) -> bool {
    let mut i = jump_i;
    while i > 0 {
        i -= 1;
        match &ops[i] {
            IlOp::Label(_) | IlOp::JoinLabel(_) => continue,
            other => return is_eq_byte(other),
        }
    }
    false
}

fn is_eq_byte(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::EQ
    ) || matches!(op, IlOp::Bin { op: Instruction::EQ, .. })
}

fn fused_eq_jmp(byte: &common::Byte) -> bool {
    match *byte.bytecode() {
        Instruction::BinSlotImmJmpf | Instruction::BinSlotImmJmpt => {
            byte.bin_slot_imm_jmpf_parts().0 == Instruction::EQ as u8
        }
        Instruction::BinSlotSlotJmpf | Instruction::BinSlotSlotJmpt => {
            byte.bin_slot_slot_jmpf_parts().0 == Instruction::EQ as u8
        }
        _ => false,
    }
}

#[cfg(test)]
mod cost_gate_tests {
    use common::{Byte, DebugLoc, Instruction};

    use crate::il::IlOp;

    use super::{emit_cost, residual_heap_box};

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn residual_make_counts_as_boxed() {
        let ops = [IlOp::MakeArray { elem_kind: 0, arity: 2, loc: loc() }];
        assert!(residual_heap_box(&ops));
        let native = [IlOp::byte(
            Byte::new(Instruction::DenseMake).with_dense_abc(0, 1, 2, 3),
        )];
        assert!(!residual_heap_box(&native));
    }

    #[test]
    fn emit_cost_weights_load_store_above_dense() {
        let boxed = [
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::Load { slot: 1, loc: loc() },
            IlOp::Index { loc: loc() },
            IlOp::StorePop { slot: 2, loc: loc() },
        ];
        let native = [IlOp::byte(
            Byte::new(Instruction::DenseIndex).with_dense_abc(0, 2, 0, 1),
        )];
        assert!(emit_cost(&native) < emit_cost(&boxed));
        assert!(emit_cost(&native) <= emit_cost(&boxed));
    }

    #[test]
    fn emit_cost_weights_seek_by_frame_size() {
        let small = [IlOp::byte(
            Byte::new(Instruction::Seek).with_operand_u32(8),
        )];
        let large = [IlOp::byte(
            Byte::new(Instruction::Seek).with_operand_u32(80),
        )];
        assert!(emit_cost(&large) > emit_cost(&small));
    }

    /// COI-385: packed dense ops cost 1 — same as any non-Seek/LOAD Byte.
    #[test]
    fn emit_cost_does_not_tax_packed_dense_ops() {
        let dense = [
            IlOp::byte(Byte::new(Instruction::DenseBin).with_dense_abc(0, 2, 0, 1)),
            IlOp::byte(Byte::new(Instruction::DenseMove).with_dense_abc(0, 1, 0, 0)),
        ];
        assert_eq!(emit_cost(&dense), 2);
    }
}
