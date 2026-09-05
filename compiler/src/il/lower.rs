//! IL → `Vec<Byte>` lowering: one [`fuse_select`] then PC assign.
//!
//! Fuse windows are typed [`IlOp`]. Residual [`IlOp::Byte`] is a cold refuse.
//! Labels / [`IlOp::JoinLabel`] / [`FuseHint`] are hard barriers (D3). No
//! post-lower `adjust_target`. Per-function fuse is not production.

use std::collections::HashMap;

use common::{Byte, DebugLoc, Instruction};

use super::builder::IlError;
#[cfg(test)]
use super::func::IlFunc;
use super::op::{EntryKind, FuseHint, IlJumpKind, IlOp, Label};
use super::opt;

/// Result of lowering an IL module.
pub struct Lowered {
    pub bytecode: Vec<Byte>,
    pub debug_locs: Vec<DebugLoc>,
    /// Final PC for each bound label (last bind wins).
    pub label_pcs: HashMap<u32, usize>,
    /// Pre-fusion emitting index → post-fusion PC (for remapping `functions` etc.).
    pub pre_to_post: HashMap<usize, usize>,
    /// Cumulative emit-label → flat-IL label remap from [`super::IlModule::to_flat`].
    pub label_remap: HashMap<u32, u32>,
    /// Per-function chunk remaps from [`super::IlModule::to_flat`] (same order as
    /// [`super::CodeBuf::funcs`]); use for entry-label PC resolution.
    pub func_label_maps: Vec<HashMap<u32, u32>>,
    /// Post-fusion bytecode length.
    pub code_len: usize,
    /// Post-opt, pre-fuse ops when captured for the cursor_model gate.
    pub pre_fuse_ops: Option<Vec<IlOp>>,
}

/// Intermediate slot before PC assignment. Jump targets stay symbolic.
#[derive(Clone)]
enum Slot {
    /// Encoded from a **typed** [`IlOp`] (Load/Const/Bin/…). Fusable.
    Byte(Byte, DebugLoc),
    /// Residual [`IlOp::Byte`] cold set. Fuse-select refuses any window that includes it.
    Cold(Byte, DebugLoc),
    Jump(IlJumpKind, Label, DebugLoc, FuseHint),
    /// `EntryKind::Call` return width (`1` or `2` words); other kinds are `1`.
    Entry(EntryKind, u32, Label, DebugLoc, u32),
    PrologueJmp(DebugLoc),
    CmpJmpf(u8, Label, DebugLoc, bool),
    LogNotJmpf(Label, DebugLoc, bool),
    BinSlotImmJmpf {
        op: u8,
        slot: u8,
        imm: i16,
        target: Label,
        loc: DebugLoc,
        if_true: bool,
    },
    BinSlotSlotJmpf {
        op: u8,
        a: u8,
        b: u8,
        target: Label,
        loc: DebugLoc,
            if_true: bool,
        },
}

impl Slot {
    fn loc(&self) -> DebugLoc {
        match self {
            Slot::Byte(_, l)
            | Slot::Cold(_, l)
            | Slot::Jump(_, _, l, _)
            | Slot::Entry(_, _, _, l, _)
            | Slot::PrologueJmp(l)
            | Slot::CmpJmpf(_, _, l, _)
            | Slot::LogNotJmpf(_, l, _)
            | Slot::BinSlotImmJmpf { loc: l, .. }
            | Slot::BinSlotSlotJmpf { loc: l, .. } => *l,
        }
    }
}

/// True if `op` is a plain `IlOp::Byte` JMP/JMPF/JMPT carrying a real absolute
/// PC (not the prologue sentinel `u32::MAX`). Symbolic jumps use [`IlOp::Jump`].
pub fn is_residual_abs_jump(op: &IlOp) -> bool {
    let IlOp::Byte { byte, .. } = op else {
        return false;
    };
    matches!(
        *byte.bytecode(),
        Instruction::JMP | Instruction::JMPF | Instruction::JMPT
    ) && byte.operand_u32() != u32::MAX
}

/// Debug/test inventory: panics if any residual absolute control-flow jump
/// remains as `IlOp::Byte`. Allow [`IlOp::PrologueJmp`] / `u32::MAX` only.
pub fn assert_no_residual_abs_jumps(ops: &[IlOp]) {
    for (i, op) in ops.iter().enumerate() {
        debug_assert!(
            !is_residual_abs_jump(op),
            "residual abs JMP/JMPF/JMPT as IlOp::Byte at op index {i}; labelize before opts/fuse"
        );
        if is_residual_abs_jump(op) {
            panic!(
                "residual abs JMP/JMPF/JMPT as IlOp::Byte at op index {i}; labelize before opts/fuse"
            );
        }
    }
}

/// Optimize and lower `ops` into VM bytecode.
///
/// When `funcs` is empty, opts run on the whole buffer (unit tests). Production
/// uses [`super::CodeBuf::lower_in_place`] → [`lower_module_inner`].
#[cfg(test)]
pub fn lower(ops: &[IlOp], pool: &mut Vec<u64>) -> Lowered {
    try_lower(ops, pool).unwrap_or_else(|e| panic!("{e}"))
}

/// Lower `ops`, returning [`IlError::UnboundLabel`] instead of emitting JMP to PC 0.
#[cfg(test)]
pub fn try_lower(ops: &[IlOp], pool: &mut Vec<u64>) -> Result<Lowered, IlError> {
    try_lower_with_funcs(ops, &[], pool)
}

/// Rebuild [`super::IlModule`] from flat ops + spans, then [`lower_module`].
#[cfg(test)]
pub fn lower_with_funcs(ops: &[IlOp], funcs: &[IlFunc], pool: &mut Vec<u64>) -> Lowered {
    try_lower_with_funcs(ops, funcs, pool).unwrap_or_else(|e| panic!("{e}"))
}

#[cfg(test)]
fn try_lower_with_funcs(
    ops: &[IlOp],
    funcs: &[IlFunc],
    pool: &mut Vec<u64>,
) -> Result<Lowered, IlError> {
    let mut module = super::IlModule::from_flat(ops, funcs);
    try_lower_module(&mut module, pool)
}

/// Optimize an owning [`super::IlModule`] and lower once (fuse-select + PC assign).
///
/// Pipeline: per-body opts/GVN → concat → whole-buffer multi_op → single lower.
#[cfg(test)]
pub fn lower_module(module: &mut super::IlModule, pool: &mut Vec<u64>) -> Lowered {
    try_lower_module(module, pool).unwrap_or_else(|e| panic!("{e}"))
}

#[cfg(test)]
fn try_lower_module(
    module: &mut super::IlModule,
    pool: &mut Vec<u64>,
) -> Result<Lowered, IlError> {
    lower_module_inner(module, pool, false, &opt::OptimizeOptions::default())
}

/// Like [`lower_module`], optionally retaining the post-opt pre-fuse op stream.
pub(crate) fn lower_module_inner(
    module: &mut super::IlModule,
    pool: &mut Vec<u64>,
    capture_ops: bool,
    opts: &opt::OptimizeOptions,
) -> Result<Lowered, IlError> {
    super::bounds::reset_bounds_stats();
    super::canon::reset_canon_stats();
    let (flat, label_remap, func_label_maps) = module.optimize_and_flatten(opts, pool);
    let mut lowered = try_lower_optimized(&flat, pool)?;
    lowered.label_remap = label_remap;
    lowered.func_label_maps = func_label_maps;
    if capture_ops {
        lowered.pre_fuse_ops = Some(flat);
    }
    Ok(lowered)
}

/// Fuse-select + PC assign for an already-optimized op stream (no IL opts).
#[cfg(test)]
pub(crate) fn lower_optimized(ops: &[IlOp], pool: &mut Vec<u64>) -> Lowered {
    try_lower_optimized(ops, pool).unwrap_or_else(|e| panic!("{e}"))
}

fn try_lower_optimized(ops: &[IlOp], pool: &mut Vec<u64>) -> Result<Lowered, IlError> {
    let FuseOut {
        slots,
        binds_at,
        end_labels,
        pre_len,
        pre_to_post,
    } = fuse_select(ops, pool);

    // Assign in pre-slot order so a rebound label keeps the *last* bind
    // (HashMap iteration order would be nondeterministic).
    let mut label_pcs: HashMap<u32, usize> = HashMap::new();
    for pre in 0..pre_len {
        if let Some(ids) = binds_at.get(&pre) {
            let pc = pre_to_post.get(&pre).copied().unwrap_or(slots.len());
            for id in ids {
                label_pcs.insert(*id, pc);
            }
        }
    }
    for id in end_labels {
        label_pcs.insert(id, slots.len());
    }

    let mut bytecode = Vec::with_capacity(slots.len());
    let mut debug_locs = Vec::with_capacity(slots.len());
    for slot in &slots {
        bytecode.push(encode_slot(slot, &label_pcs, pool)?);
        debug_locs.push(slot.loc());
    }

    // Symbolic jumps are label-resolved at encode. Residual abs JMP Bytes
    // are forbidden ([`assert_no_residual_abs_jumps`]); this hook is a no-op.
    remap_absolute_targets(&mut bytecode, pool, &pre_to_post, slots.len());

    let code_len = bytecode.len();
    Ok(Lowered {
        bytecode,
        debug_locs,
        label_pcs,
        pre_to_post,
        code_len,
        label_remap: HashMap::new(),
        func_label_maps: Vec::new(),
        pre_fuse_ops: None,
    })
}

/// Named fuse-select pass: typed [`IlOp`] windows, then fused [`Slot`]s.
///
/// Called once from [`lower_optimized`] after concat. Not a second lowering:
/// PC assign / encode still happen in [`lower_optimized`]. Residual
/// [`IlOp::Byte`] is [`Slot::Cold`] — refused in any multi-op window.
/// [`IlOp::Label`] / [`IlOp::JoinLabel`] binds and [`FuseHint`] are barriers
/// (D3 metadata, no dummy ops). Per-function fuse is not production.
pub(crate) fn fuse_select(ops: &[IlOp], pool: &mut Vec<u64>) -> FuseOut {
    assert_no_residual_abs_jumps(ops);

    let mut pre_slots: Vec<Slot> = Vec::with_capacity(ops.len());
    let mut binds_at: HashMap<usize, Vec<u32>> = HashMap::new();
    let mut pending: Vec<u32> = Vec::new();

    for op in ops {
        match op {
            IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => pending.push(*id),
            IlOp::Jump {
                kind,
                target,
                loc,
                hint,
            } => {
                let idx = pre_slots.len();
                if !pending.is_empty() {
                    binds_at.insert(idx, std::mem::take(&mut pending));
                }
                pre_slots.push(Slot::Jump(*kind, *target, *loc, *hint));
            }
            IlOp::Entry {
                kind,
                arity,
                target,
                loc,
                ret_words,
            } => {
                let idx = pre_slots.len();
                if !pending.is_empty() {
                    binds_at.insert(idx, std::mem::take(&mut pending));
                }
                pre_slots.push(Slot::Entry(*kind, *arity, *target, *loc, *ret_words));
            }
            IlOp::PrologueJmp { loc } => {
                let idx = pre_slots.len();
                if !pending.is_empty() {
                    binds_at.insert(idx, std::mem::take(&mut pending));
                }
                pre_slots.push(Slot::PrologueJmp(*loc));
            }
            IlOp::Byte { byte, loc } => {
                let idx = pre_slots.len();
                if !pending.is_empty() {
                    binds_at.insert(idx, std::mem::take(&mut pending));
                }
                pre_slots.push(Slot::Cold(*byte, *loc));
            }
            other => {
                let Some(byte) = other.as_encode_byte() else {
                    continue;
                };
                let idx = pre_slots.len();
                if !pending.is_empty() {
                    binds_at.insert(idx, std::mem::take(&mut pending));
                }
                pre_slots.push(Slot::Byte(byte, other.loc()));
            }
        }
    }
    let end_labels = pending;
    let pre_len = pre_slots.len();
    let (slots, pre_to_post) = fuse_slots_with_origins(pre_slots, pool, &binds_at);
    FuseOut {
        slots,
        binds_at,
        end_labels,
        pre_len,
        pre_to_post,
    }
}

pub(crate) struct FuseOut {
    slots: Vec<Slot>,
    binds_at: HashMap<usize, Vec<u32>>,
    end_labels: Vec<u32>,
    pre_len: usize,
    pre_to_post: HashMap<usize, usize>,
}

fn fuse_slots_with_origins(
    slots: Vec<Slot>,
    pool: &mut Vec<u64>,
    binds_at: &HashMap<usize, Vec<u32>>,
) -> (Vec<Slot>, HashMap<usize, usize>) {
    let abs_jump_targets = absolute_jump_targets(&slots);
    let mut out = Vec::with_capacity(slots.len());
    let mut origins: HashMap<usize, usize> = HashMap::new();
    let mut i = 0;
    while i < slots.len() {
        // Do not fuse a window that would pull an op with an incoming
        // label / absolute jump into a fused superinstruction with a
        // preceding op (match joins, attr-inlined absolute JMP→RETURN).
        // *Return fusions refuse an *unconditional* join on window[0]:
        // `JMP` there can carry a stacked arm value that `RETURN` must
        // pop, while `ConstReturnImm` would ignore it. Compare-jumps
        // (`*Jmpf`/`*Jmpt`) do not leave that value, so a labeled
        // `CONST; RETURN` (fib `if n <= 2 { return 1 }`) may fuse.
        let mut fused = None;
        if let Some((f, window)) = try_fuse_slots(&slots[i..], pool) {
            let crosses_label = (1..window).any(|k| binds_at.contains_key(&(i + k)));
            let crosses_abs = (1..window).any(|k| abs_jump_targets.contains(&(i + k)));
            let has_cold = (0..window).any(|k| matches!(slots[i + k], Slot::Cold(..)));
            let return_at_uncond_join =
                slot_is_return_fusion(&f) && join_has_unconditional_pred(&slots, i, binds_at);
            if !crosses_label && !crosses_abs && !has_cold && !return_at_uncond_join {
                fused = Some((f, window));
            }
        }
        if let Some((fused, window)) = fused {
            let post = out.len();
            for k in 0..window {
                origins.insert(i + k, post);
            }
            out.push(fused);
            i += window;
        } else {
            origins.insert(i, out.len());
            out.push(slots[i].clone());
            i += 1;
        }
    }
    (out, origins)
}

/// Pre-fusion indices targeted by absolute `JMP`/`JMPF`/`JMPT` bytes.
fn absolute_jump_targets(slots: &[Slot]) -> std::collections::HashSet<usize> {
    let mut set = std::collections::HashSet::new();
    for s in slots {
        let Slot::Byte(b, _) = s else {
            continue;
        };
        match *b.bytecode() {
            Instruction::JMP | Instruction::JMPF | Instruction::JMPT => {
                let t = b.operand_u32();
                if t != u32::MAX {
                    set.insert(t as usize);
                }
            }
            _ => {}
        }
    }
    set
}

fn cond_jump(slot: &Slot) -> Option<(bool, Label, FuseHint)> {
    match slot {
        Slot::Jump(IlJumpKind::JumpIfFalse, t, _, h) => Some((false, *t, *h)),
        Slot::Jump(IlJumpKind::JumpIfTrue, t, _, h) => Some((true, *t, *h)),
        _ => None,
    }
}

fn cond_jump_fusable(slot: &Slot) -> Option<(bool, Label)> {
    let (if_true, tgt, hint) = cond_jump(slot)?;
    if hint.blocks_cmp_jmp_fuse() {
        None
    } else {
        Some((if_true, tgt))
    }
}

fn try_fuse_slots(window: &[Slot], pool: &mut Vec<u64>) -> Option<(Slot, usize)> {
    // Fuse-select order mirrors historical peephole try_fuse; JMPF targets stay symbolic.
    // Mandelbrot-shaped `FloatChainStore` / `BinSlotSlotConstJmpf` are not emitted.
    if let Some(s) = try_fuse_load_const_cmp_jmpf_slot(window) {
        return Some((s, 4));
    }
    if let Some(s) = try_fuse_load_load_op_jmpf_slot(window) {
        return Some((s, 4));
    }
    if let Some(s) = try_fuse_load_const_op_store(window, pool) {
        return Some((s, 4));
    }
    if let Some(s) = try_fuse_load_load_op_store(window) {
        return Some((s, 4));
    }
    if window.len() >= 3
        && let (Some(a), Some(b), Some(c)) = (
            slot_as_byte(&window[0]),
            slot_as_byte(&window[1]),
            slot_as_byte(&window[2]),
        )
    {
        let w = [a, b, c];
        if let Some(fused) = try_fold_const_bin_local(&w, pool) {
            return Some((Slot::Byte(fused, window[0].loc()), 3));
        }
        if let Some(fused) = try_fuse_bin_slot_imm_local(&w) {
            return Some((Slot::Byte(fused, window[0].loc()), 3));
        }
        if let Some(fused) = try_fuse_bin_slot_slot_local(&w) {
            return Some((Slot::Byte(fused, window[0].loc()), 3));
        }
    }
    if window.len() >= 2
        && let (Some(b0), Some((if_true, tgt, jmp_hint))) =
            (slot_as_byte(&window[0]), cond_jump(&window[1]))
    {
        if !jmp_hint.blocks_cmp_jmp_fuse() {
            if *b0.bytecode() == Instruction::BinSlotImm {
                let (op, slot, imm) = b0.bin_slot_imm_parts();
                if is_jmpf_cond_op(Instruction::from(op)) {
                    return Some((
                        Slot::BinSlotImmJmpf {
                            op,
                            slot: slot as u8,
                            imm: imm as i16,
                            target: tgt,
                            loc: window[0].loc(),
                            if_true,
                        },
                        2,
                    ));
                }
            }
            if *b0.bytecode() == Instruction::BinSlotSlot {
                let (op, a, b) = b0.bin_slot_slot_parts();
                if is_jmpf_cond_op(Instruction::from(op)) {
                    return Some((
                        Slot::BinSlotSlotJmpf {
                            op,
                            a: a as u8,
                            b: b as u8,
                            target: tgt,
                            loc: window[0].loc(),
                            if_true,
                        },
                        2,
                    ));
                }
            }
            if *b0.bytecode() == Instruction::LogNot {
                return Some((Slot::LogNotJmpf(tgt, window[0].loc(), if_true), 2));
            }
            if is_jmpf_cond_op(*b0.bytecode()) {
                return Some((
                    Slot::CmpJmpf(*b0.bytecode() as u8, tgt, window[0].loc(), if_true),
                    2,
                ));
            }
        }
    }
    if window.len() >= 2
        && let (Some(a), Some(b)) = (slot_as_byte(&window[0]), slot_as_byte(&window[1]))
    {
        if let Some(fused) = try_fuse_bin_slot_imm_store_local(&a, &b, pool) {
            return Some((Slot::Byte(fused, window[0].loc()), 2));
        }
        if let Some(fused) = try_fuse_bin_slot_slot_store_local(&a, &b) {
            return Some((Slot::Byte(fused, window[0].loc()), 2));
        }
        let w = [a, b];
        if let Some(fused) = try_fuse_load_return_local(&w) {
            return Some((Slot::Byte(fused, window[0].loc()), 2));
        }
        if let Some(fused) = try_fuse_const_return_local(&w) {
            return Some((Slot::Byte(fused, window[0].loc()), 2));
        }
        if let Some(fused) = try_fuse_bin_return_local(&w) {
            return Some((Slot::Byte(fused, window[0].loc()), 2));
        }
    }
    // After patterns that consume individual LOAD/STORE: pack adjacent runs.
    if let Some((fused, n)) = try_fuse_packed_loads(window) {
        return Some((fused, n));
    }
    if let Some((fused, n)) = try_fuse_packed_stores(window) {
        return Some((fused, n));
    }
    None
}

fn try_fuse_load_const_cmp_jmpf_slot(window: &[Slot]) -> Option<Slot> {
    if window.len() < 4 {
        return None;
    }
    let b0 = slot_as_byte(&window[0])?;
    let b1 = slot_as_byte(&window[1])?;
    let b2 = slot_as_byte(&window[2])?;
    let (if_true, tgt) = cond_jump_fusable(&window[3])?;
    let slot = load_slot(&b0)?;
    let imm = i16::try_from(const_inline_value(&b1)?).ok()?;
    if !is_jmpf_cond_op(*b2.bytecode()) {
        return None;
    }
    Some(Slot::BinSlotImmJmpf {
        op: *b2.bytecode() as u8,
        slot,
        imm,
        target: tgt,
        loc: window[0].loc(),
        if_true,
    })
}

fn try_fuse_load_load_op_jmpf_slot(window: &[Slot]) -> Option<Slot> {
    if window.len() < 4 {
        return None;
    }
    let b0 = slot_as_byte(&window[0])?;
    let b1 = slot_as_byte(&window[1])?;
    let b2 = slot_as_byte(&window[2])?;
    let (if_true, tgt) = cond_jump_fusable(&window[3])?;
    let a = load_slot(&b0)?;
    let b = load_or_dup_slot(a, &b1)?;
    if !is_jmpf_cond_op(*b2.bytecode()) {
        return None;
    }
    Some(Slot::BinSlotSlotJmpf {
        op: *b2.bytecode() as u8,
        a,
        b,
        target: tgt,
        loc: window[0].loc(),
        if_true,
    })
}

/// `LOAD src; CONST imm; <int-bin>; STORE dest` → `BinSlotImmStore`.
fn try_fuse_load_const_op_store(window: &[Slot], pool: &mut Vec<u64>) -> Option<Slot> {
    if window.len() < 4 {
        return None;
    }
    let b0 = slot_as_byte(&window[0])?;
    let b1 = slot_as_byte(&window[1])?;
    let b2 = slot_as_byte(&window[2])?;
    let b3 = slot_as_byte(&window[3])?;
    let src = load_slot(&b0)?;
    let imm = i16::try_from(const_inline_value(&b1)?).ok()?;
    if !is_int_bin_op(*b2.bytecode()) {
        return None;
    }
    let dest = store_slot_u32(&b3)?;
    let idx = pool.len();
    pool.push(((dest as u64) << 32) | (imm as u16 as u32 as u64));
    Some(Slot::Byte(
        Byte::new(Instruction::BinSlotImmStore).with_bin_slot_imm_store(
            *b2.bytecode() as u8,
            src,
            idx as u16,
        ),
        window[0].loc(),
    ))
}

/// `LOAD a; LOAD b; <bin>; STORE dest` → `BinSlotSlotStore`.
fn try_fuse_load_load_op_store(window: &[Slot]) -> Option<Slot> {
    if window.len() < 4 {
        return None;
    }
    let b0 = slot_as_byte(&window[0])?;
    let b1 = slot_as_byte(&window[1])?;
    let b2 = slot_as_byte(&window[2])?;
    let b3 = slot_as_byte(&window[3])?;
    let a = load_slot(&b0)?;
    let b = load_or_dup_slot(a, &b1)?;
    if !is_bin_op(*b2.bytecode()) {
        return None;
    }
    let dest = store_slot_u8(&b3)?;
    Some(Slot::Byte(
        Byte::new(Instruction::BinSlotSlotStore).with_bin_slot_slot_store(
            *b2.bytecode() as u8,
            a,
            b,
            dest,
        ),
        window[0].loc(),
    ))
}

fn slot_as_byte(s: &Slot) -> Option<Byte> {
    match s {
        Slot::Byte(b, _) => Some(*b),
        _ => None,
    }
}

fn slot_is_return_fusion(s: &Slot) -> bool {
    match s {
        Slot::Byte(b, _) => matches!(
            *b.bytecode(),
            Instruction::LoadReturnSlot | Instruction::ConstReturnImm | Instruction::BinReturn
        ),
        _ => false,
    }
}

/// True when an unconditional `JMP` (symbolic or residual absolute) targets `i`.
fn join_has_unconditional_pred(
    slots: &[Slot],
    i: usize,
    binds_at: &HashMap<usize, Vec<u32>>,
) -> bool {
    let labels = binds_at.get(&i).map(Vec::as_slice).unwrap_or(&[]);
    for slot in slots {
        match slot {
            Slot::Jump(IlJumpKind::Unconditional, t, _, _) if labels.contains(&t.0) => return true,
            Slot::Byte(b, _)
                if *b.bytecode() == Instruction::JMP && b.operand_u32() as usize == i =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

fn encode_slot(
    slot: &Slot,
    labels: &HashMap<u32, usize>,
    pool: &mut Vec<u64>,
) -> Result<Byte, IlError> {
    Ok(match slot {
        Slot::Byte(b, _) | Slot::Cold(b, _) => *b,
        Slot::PrologueJmp(_) => Byte::new(Instruction::JMP).with_operand_u32(u32::MAX),
        Slot::Jump(kind, target, _, _) => {
            let pc = resolve(labels, *target)?;
            match kind {
                IlJumpKind::Unconditional => Byte::new(Instruction::JMP).with_operand_u32(pc),
                IlJumpKind::JumpIfFalse => Byte::new(Instruction::JMPF).with_operand_u32(pc),
                IlJumpKind::JumpIfTrue => Byte::new(Instruction::JMPT).with_operand_u32(pc),
                IlJumpKind::JumpIfMatch { tag, .. } => {
                    let idx = pool.len() as u16;
                    pool.push(pc as u64);
                    Byte::new(Instruction::JumpIfMatch).with_operands_u16([*tag as u16, idx])
                }
            }
        }
        Slot::Entry(kind, arity, target, _, ret_words) => {
            let pc = resolve(labels, *target)?;
            match kind {
                EntryKind::Call => {
                    Byte::new(Instruction::CALL).with_call_packed_ret(*arity, pc, *ret_words)
                }
                EntryKind::TailCall => {
                    Byte::new(Instruction::TailCall).with_call_packed(*arity, pc)
                }
                EntryKind::MakeCoro => {
                    Byte::new(Instruction::MakeCoro).with_call_packed(*arity, pc)
                }
                EntryKind::CodePtr => Byte::new(Instruction::CodePtr).with_operand_u32(pc),
                EntryKind::MakePolyFn => Byte::new(Instruction::MakePolyFn).with_operand_u32(pc),
            }
        }
        Slot::CmpJmpf(op, target, _, if_true) => {
            let pc = resolve(labels, *target)?;
            let insn = if *if_true {
                Instruction::CmpJmpt
            } else {
                Instruction::CmpJmpf
            };
            if pc <= u16::MAX as u32 {
                Byte::new(insn).with_cmp_jmpf(*op, pc as u16)
            } else {
                let idx = pool.len();
                pool.push(pc as u64);
                Byte::new(insn).with_cmp_jmpf_pool(*op, idx as u16)
            }
        }
        Slot::LogNotJmpf(target, _, if_true) => {
            let pc = resolve(labels, *target)?;
            let insn = if *if_true {
                Instruction::LogNotJmpt
            } else {
                Instruction::LogNotJmpf
            };
            if pc <= u16::MAX as u32 {
                Byte::new(insn).with_log_not_jmpf(pc as u16)
            } else {
                let idx = pool.len();
                pool.push(pc as u64);
                Byte::new(insn).with_log_not_jmpf_pool(idx as u16)
            }
        }
        Slot::BinSlotImmJmpf {
            op,
            slot,
            imm,
            target,
            if_true,
            ..
        } => {
            let pc = resolve(labels, *target)?;
            let idx = pool.len();
            pool.push(((pc as u64) << 32) | (*imm as u16 as u32 as u64));
            let insn = if *if_true {
                Instruction::BinSlotImmJmpt
            } else {
                Instruction::BinSlotImmJmpf
            };
            Byte::new(insn).with_bin_slot_imm_jmpf(*op, *slot, idx as u16)
        }
        Slot::BinSlotSlotJmpf {
            op,
            a,
            b,
            target,
            if_true,
            ..
        } => {
            let pc = resolve(labels, *target)?;
            let idx = pool.len();
            pool.push(((pc as u64) << 32) | (*b as u64));
            let insn = if *if_true {
                Instruction::BinSlotSlotJmpt
            } else {
                Instruction::BinSlotSlotJmpf
            };
            Byte::new(insn).with_bin_slot_slot_jmpf(*op, *a, idx as u16)
        }
    })
}

fn resolve(labels: &HashMap<u32, usize>, target: Label) -> Result<u32, IlError> {
    labels
        .get(&target.0)
        .copied()
        .map(|pc| pc as u32)
        .ok_or(IlError::UnboundLabel(target))
}

/// Remap residual absolute jump targets that still use pre-fusion indices.
/// Symbolic [`IlOp::Jump`] / fused jmp forms are already label-resolved at
/// encode — do not touch them (double-remap breaks loop exits under fusion).
/// Leftover CALL/CodePtr Bytes (missing fn CodePtr 0) are not fusion-sensitive
/// and are left as-is.
fn remap_absolute_targets(
    bytecode: &mut [Byte],
    pool: &mut [u64],
    pre_to_post: &HashMap<usize, usize>,
    len: usize,
) {
    let _ = (bytecode, pool, pre_to_post, len);
    // Production emit has no residual abs JMP/JMPF/JMPT as `IlOp::Byte`
    // (see [`assert_no_residual_abs_jumps`]). Prologue uses `u32::MAX`.
}

fn is_int_bin_op(i: Instruction) -> bool {
    matches!(
        i,
        Instruction::ADD
            | Instruction::SUB
            | Instruction::MUL
            | Instruction::DIV
            | Instruction::MOD
            | Instruction::LE
            | Instruction::LEQ
            | Instruction::GT
            | Instruction::GEQ
            | Instruction::EQ
            | Instruction::NEQ
            | Instruction::Pow
            | Instruction::BITAND
            | Instruction::BITOR
            | Instruction::SHL
            | Instruction::SHR
            | Instruction::XOR
            | Instruction::AND
            | Instruction::OR
    )
}

fn is_cmp_op(i: Instruction) -> bool {
    matches!(
        i,
        Instruction::LE
            | Instruction::LEQ
            | Instruction::GT
            | Instruction::GEQ
            | Instruction::EQ
            | Instruction::NEQ
            | Instruction::LEF
            | Instruction::LEQF
            | Instruction::GTF
            | Instruction::GEQF
    )
}

/// Ops whose result is a JMPF condition (cmp, logical, or bitwise mask).
/// Condition opcodes that fuse into a `*Jmpf` superinstruction with a following
/// `JumpIfFalse`. `opt::cfg` consults this to avoid inverting a fusable guard.
pub(crate) fn is_jmpf_cond_op(i: Instruction) -> bool {
    is_cmp_op(i)
        || matches!(
            i,
            Instruction::AND
                | Instruction::OR
                | Instruction::BITAND
                | Instruction::BITOR
                | Instruction::XOR
        )
}

fn is_bin_op(i: Instruction) -> bool {
    is_int_bin_op(i)
        || matches!(
            i,
            Instruction::ADDF
                | Instruction::SUBF
                | Instruction::MULF
                | Instruction::DIVF
                | Instruction::MODF
                | Instruction::LEF
                | Instruction::LEQF
                | Instruction::GTF
                | Instruction::GEQF
                | Instruction::PowF
        )
}

fn const_inline_value(byte: &Byte) -> Option<i32> {
    if *byte.bytecode() != Instruction::CONST {
        return None;
    }
    let op = byte.operand_u32();
    if op & Byte::POOL_FLAG != 0 {
        return None;
    }
    Some(op as i32)
}

fn load_slot(byte: &Byte) -> Option<u8> {
    if *byte.bytecode() != Instruction::LOAD {
        return None;
    }
    let slot = byte.load_store_single_slot()?;
    if slot > 255 {
        return None;
    }
    Some(slot as u8)
}

/// `DUP` copies TOS; after `LOAD a` that is slot `a` again.
fn load_or_dup_slot(first: u8, byte: &Byte) -> Option<u8> {
    if let Some(s) = load_slot(byte) {
        return Some(s);
    }
    if *byte.bytecode() == Instruction::DUPLICATE {
        return Some(first);
    }
    None
}

fn store_slot_u8(byte: &Byte) -> Option<u8> {
    if !matches!(*byte.bytecode(), Instruction::STORE | Instruction::StorePop) {
        return None;
    }
    let slot = byte.load_store_single_slot()?;
    if slot > 255 {
        return None;
    }
    Some(slot as u8)
}

fn store_slot_u32(byte: &Byte) -> Option<u32> {
    if !matches!(*byte.bytecode(), Instruction::STORE | Instruction::StorePop) {
        return None;
    }
    byte.load_store_single_slot()
}

/// Adjacent `LOAD`×2/3 → one packed `LOAD` (`n` in `[31:24]`).
fn try_fuse_packed_loads(window: &[Slot]) -> Option<(Slot, usize)> {
    for n in [3usize, 2] {
        if window.len() < n {
            continue;
        }
        let mut slots = [0u8; 3];
        let mut ok = true;
        for i in 0..n {
            let Some(b) = slot_as_byte(&window[i]) else {
                ok = false;
                break;
            };
            let Some(s) = load_slot(&b) else {
                ok = false;
                break;
            };
            slots[i] = s;
        }
        if ok {
            return Some((
                Slot::Byte(
                    Byte::new(Instruction::LOAD)
                        .with_load_store_packed(n as u8, slots[0], slots[1], slots[2]),
                    window[0].loc(),
                ),
                n,
            ));
        }
    }
    None
}

/// Adjacent `STORE`×2/3 → one packed `STORE` (TOS → first listed slot).
fn try_fuse_packed_stores(window: &[Slot]) -> Option<(Slot, usize)> {
    for n in [3usize, 2] {
        if window.len() < n {
            continue;
        }
        let mut slots = [0u8; 3];
        let mut ok = true;
        for i in 0..n {
            let Some(b) = slot_as_byte(&window[i]) else {
                ok = false;
                break;
            };
            let Some(s) = store_slot_u8(&b) else {
                ok = false;
                break;
            };
            slots[i] = s;
        }
        if ok {
            return Some((
                Slot::Byte(
                    Byte::new(Instruction::STORE)
                        .with_load_store_packed(n as u8, slots[0], slots[1], slots[2]),
                    window[0].loc(),
                ),
                n,
            ));
        }
    }
    None
}

fn try_fuse_bin_slot_imm_local(window: &[Byte; 3]) -> Option<Byte> {
    let slot = load_slot(&window[0])?;
    let imm = i16::try_from(const_inline_value(&window[1])?).ok()?;
    let op = *window[2].bytecode();
    if !is_int_bin_op(op) {
        return None;
    }
    Some(Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(op as u8, slot, imm))
}

fn try_fuse_bin_slot_slot_local(window: &[Byte; 3]) -> Option<Byte> {
    let a = load_slot(&window[0])?;
    let b = load_or_dup_slot(a, &window[1])?;
    let op = *window[2].bytecode();
    if !is_bin_op(op) {
        return None;
    }
    Some(Byte::new(Instruction::BinSlotSlot).with_bin_slot_slot(op as u8, a, b))
}

/// `BinSlotImm; STORE dest` → `BinSlotImmStore`.
fn try_fuse_bin_slot_imm_store_local(b0: &Byte, b1: &Byte, pool: &mut Vec<u64>) -> Option<Byte> {
    if *b0.bytecode() != Instruction::BinSlotImm {
        return None;
    }
    let (op, src, imm) = b0.bin_slot_imm_parts();
    if !is_int_bin_op(Instruction::from(op)) {
        return None;
    }
    let dest = store_slot_u32(b1)?;
    let idx = pool.len();
    pool.push(((dest as u64) << 32) | (imm as i16 as u16 as u32 as u64));
    Some(Byte::new(Instruction::BinSlotImmStore).with_bin_slot_imm_store(op, src as u8, idx as u16))
}

/// `BinSlotSlot; STORE dest` → `BinSlotSlotStore`.
fn try_fuse_bin_slot_slot_store_local(b0: &Byte, b1: &Byte) -> Option<Byte> {
    if *b0.bytecode() != Instruction::BinSlotSlot {
        return None;
    }
    let (op, a, b) = b0.bin_slot_slot_parts();
    if !is_bin_op(Instruction::from(op)) {
        return None;
    }
    let dest = store_slot_u8(b1)?;
    Some(
        Byte::new(Instruction::BinSlotSlotStore)
            .with_bin_slot_slot_store(op, a as u8, b as u8, dest),
    )
}

fn try_fold_const_bin_local(window: &[Byte; 3], pool: &mut Vec<u64>) -> Option<Byte> {
    let a = const_inline_value(&window[0])? as i64;
    let b = const_inline_value(&window[1])? as i64;
    let result = match *window[2].bytecode() {
        Instruction::ADD => a + b,
        Instruction::SUB => a - b,
        Instruction::MUL => a * b,
        Instruction::DIV if b != 0 => a / b,
        Instruction::MOD if b != 0 => a % b,
        _ => return None,
    };
    if (0..=i32::MAX as i64).contains(&result) {
        return Some(Byte::new(Instruction::CONST).with_operand_u32(result as u32));
    }
    let bits = common::Value::from(result).raw() as u64;
    let idx = pool.len();
    pool.push(bits);
    Some(Byte::new(Instruction::CONST).with_const_pool(idx as u32))
}

/// True for a one-word `RETURN` (default / old archives). A two-slot
/// `RETURN` (operand `2`) must never fuse into `*ReturnImm`/`BinReturn` —
/// those forms return exactly one value and would silently drop the
/// `[payload, tag]` pair's other half.
fn is_one_word_return(byte: &Byte) -> bool {
    *byte.bytecode() == Instruction::RETURN && byte.return_words() == 1
}

fn try_fuse_load_return_local(window: &[Byte; 2]) -> Option<Byte> {
    let slot = load_slot(&window[0])?;
    if !is_one_word_return(&window[1]) {
        return None;
    }
    Some(Byte::new(Instruction::LoadReturnSlot).with_operand_u32(slot as u32))
}

fn try_fuse_const_return_local(window: &[Byte; 2]) -> Option<Byte> {
    let value = const_inline_value(&window[0])?;
    if !is_one_word_return(&window[1]) {
        return None;
    }
    Some(Byte::new(Instruction::ConstReturnImm).with_operand_u32(value as u32))
}

fn try_fuse_bin_return_local(window: &[Byte; 2]) -> Option<Byte> {
    let op = *window[0].bytecode();
    if !is_bin_op(op) {
        return None;
    }
    if !is_one_word_return(&window[1]) {
        return None;
    }
    Some(Byte::new(Instruction::BinReturn).with_bin_return(op as u8))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::IlBuilder;
    use common::Value;

    #[test]
    fn lower_resolves_forward_jmp() {
        let mut il = IlBuilder::new();
        let end = il.fresh_label();
        il.emit_jump(IlJumpKind::Unconditional, end);
        // Live code after JMP must be entered via a label (thunk style).
        il.bind_label(end);
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(1));
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert_eq!(lowered.bytecode.len(), 3);
        assert_eq!(lowered.bytecode[0].operand_u32(), 1);
        assert!(matches!(*lowered.bytecode[2].bytecode(), Instruction::HALT));
    }

    #[test]
    fn missing_bind_fails_compile_and_does_not_jmp_pc_zero() {
        let mut il = IlBuilder::new();
        let missing = il.fresh_label();
        il.emit_jump(IlJumpKind::Unconditional, missing);
        il.push_byte(Byte::new(Instruction::HALT));

        assert!(matches!(
            il.finalize_labels(),
            Err(IlError::UnboundLabel(l)) if l == missing
        ));

        let mut pool = Vec::new();
        let Err(err) = try_lower(il.ops(), &mut pool) else {
            panic!("unbound label must fail compile, not emit JMP 0");
        };
        assert!(matches!(err, IlError::UnboundLabel(l) if l == missing));
        // No Lowered bytecode, so no JMP targeting PC 0.
    }

    #[test]
    fn lower_fuses_bin_slot_slot() {
        let mut il = IlBuilder::new();
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(0));
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(1));
        il.push_byte(Byte::new(Instruction::ADD));
        il.push_byte(Byte::new(Instruction::RETURN));
        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert_eq!(lowered.bytecode.len(), 2);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::BinSlotSlot
        ));
        assert!(matches!(
            *lowered.bytecode[1].bytecode(),
            Instruction::RETURN
        ));
    }

    #[test]
    fn lower_fuses_bin_slot_imm() {
        let mut il = IlBuilder::new();
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(0));
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(1));
        il.push_byte(Byte::new(Instruction::SUB));
        il.push_byte(Byte::new(Instruction::HALT));
        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert_eq!(lowered.bytecode.len(), 2);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::BinSlotImm
        ));
        assert_eq!(
            lowered.bytecode[0].bin_slot_imm_parts(),
            (Instruction::SUB as u8, 0, 1)
        );
    }

    #[test]
    fn lower_fuses_const_return_imm() {
        let mut il = IlBuilder::new();
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(7));
        il.push_byte(Byte::new(Instruction::RETURN));
        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert_eq!(lowered.bytecode.len(), 1);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::ConstReturnImm
        ));
        assert_eq!(lowered.bytecode[0].operand_u32(), 7);
    }

    #[test]
    fn lower_fuses_load_return_slot() {
        let mut il = IlBuilder::new();
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(3));
        il.push_byte(Byte::new(Instruction::RETURN));
        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert_eq!(lowered.bytecode.len(), 1);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::LoadReturnSlot
        ));
        assert_eq!(lowered.bytecode[0].operand_u32(), 3);
    }

    #[test]
    fn lower_fuses_bin_return() {
        let mut il = IlBuilder::new();
        il.push_byte(Byte::new(Instruction::ADD));
        il.push_byte(Byte::new(Instruction::RETURN));
        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert_eq!(lowered.bytecode.len(), 1);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::BinReturn
        ));
        assert_eq!(lowered.bytecode[0].bin_return_op(), Instruction::ADD as u8);
    }

    #[test]
    fn lower_resolves_jmpf_with_label() {
        let mut il = IlBuilder::new();
        let exit = il.fresh_label();
        il.push_byte(Byte::new(Instruction::EQ));
        il.emit_jump(IlJumpKind::JumpIfFalse, exit);
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(1));
        il.bind_label(exit);
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        // EQ; JMPF fuses to CmpJmpf (target still label-resolved).
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::CmpJmpf
        ));
        assert_eq!(lowered.bytecode[0].cmp_jmpf_parts().1, 2);
    }

    #[test]
    fn lower_refuses_cmp_jmpf_when_jump_is_nofuse() {
        let mut il = IlBuilder::new();
        let exit = il.fresh_label();
        il.push_byte(Byte::new(Instruction::EQ));
        il.emit_jump_hinted(
            IlJumpKind::JumpIfFalse,
            exit,
            common::DebugLoc::unknown(),
            FuseHint::nofuse_value_under_jmp(),
        );
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(1));
        il.bind_label(exit);
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = Vec::new();
        let lowered = lower_optimized(il.ops(), &mut pool);
        let ops: Vec<_> = lowered.bytecode.iter().map(|b| *b.bytecode()).collect();
        assert!(
            !ops.iter().any(|o| matches!(o, Instruction::CmpJmpf)),
            "nofuse JMPF must stay unfused; got {ops:?}"
        );
        assert!(matches!(ops[0], Instruction::EQ));
        assert!(matches!(ops[1], Instruction::JMPF));
    }

    #[test]
    fn lower_refuses_const_return_across_value_join() {
        let mut il = IlBuilder::new();
        let join = il.fresh_label();
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(2));
        il.emit_jump(IlJumpKind::Unconditional, join);
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(1));
        il.bind_join_label(join);
        il.push_byte(Byte::new(Instruction::RETURN));

        let mut pool = Vec::new();
        let lowered = lower_optimized(il.ops(), &mut pool);
        let ops: Vec<_> = lowered.bytecode.iter().map(|b| *b.bytecode()).collect();
        assert!(
            !ops.iter().any(|o| matches!(o, Instruction::ConstReturnImm)),
            "value join must block CONST;RETURN fuse; got {ops:?}"
        );
        assert!(
            ops.iter().any(|o| matches!(o, Instruction::JMP)),
            "other arm must keep JMP to join; got {ops:?}"
        );
    }

    #[test]
    fn lower_fuses_log_not_jmpf() {
        let mut il = IlBuilder::new();
        let exit = il.fresh_label();
        il.push_byte(Byte::new(Instruction::LogNot));
        il.emit_jump(IlJumpKind::JumpIfFalse, exit);
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(1));
        il.bind_label(exit);
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::LogNotJmpf
        ));
        assert_eq!(lowered.bytecode[0].log_not_jmpf_target(), 2);
    }

    #[test]
    fn lower_fuses_load_const_cmp_jmpf_to_bin_slot_imm_jmpf() {
        let mut il = IlBuilder::new();
        let exit = il.fresh_label();
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(0));
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(2));
        il.push_byte(Byte::new(Instruction::LE));
        il.emit_jump(IlJumpKind::JumpIfFalse, exit);
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(1));
        il.bind_label(exit);
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::BinSlotImmJmpf
        ));
        let (op, slot, idx) = lowered.bytecode[0].bin_slot_imm_jmpf_parts();
        assert_eq!(op, Instruction::LE as u8);
        assert_eq!(slot, 0);
        // Pool entry: (pc << 32) | imm; false branch is CONST;HALT → PC 2.
        let packed = pool[idx];
        assert_eq!(packed >> 32, 2);
        assert_eq!(packed as u16, 2);
    }

    #[test]
    fn lower_fuses_load_load_cmp_jmpf_to_bin_slot_slot_jmpf() {
        let mut il = IlBuilder::new();
        let exit = il.fresh_label();
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(0));
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(1));
        il.push_byte(Byte::new(Instruction::LE));
        il.emit_jump(IlJumpKind::JumpIfFalse, exit);
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(1));
        il.bind_label(exit);
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::BinSlotSlotJmpf
        ));
        let (op, a, idx) = lowered.bytecode[0].bin_slot_slot_jmpf_parts();
        assert_eq!(op, Instruction::LE as u8);
        assert_eq!(a, 0);
        let packed = pool[idx];
        assert_eq!(packed >> 32, 2);
        assert_eq!(packed as u8, 1); // slot b
    }

    #[test]
    fn lower_fuses_bin_slot_slot_and_jmpf() {
        let mut il = IlBuilder::new();
        let exit = il.fresh_label();
        il.push_byte(Byte::new(Instruction::BinSlotSlot).with_bin_slot_slot(
            Instruction::AND as u8,
            0,
            1,
        ));
        il.emit_jump(IlJumpKind::JumpIfFalse, exit);
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(1));
        il.bind_label(exit);
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::BinSlotSlotJmpf
        ));
        let (op, a, idx) = lowered.bytecode[0].bin_slot_slot_jmpf_parts();
        assert_eq!(op, Instruction::AND as u8);
        assert_eq!(a, 0);
        assert_eq!(pool[idx] as u8, 1);
    }

    #[test]
    fn lower_fuses_bin_slot_slot_const_jmpf_escape() {
        // Mandelbrot escape: BinSlotSlot ADDF; CONST pool 4.0; GTF; JMPF
        let mut il = IlBuilder::new();
        let exit = il.fresh_label();
        il.push_byte(Byte::new(Instruction::BinSlotSlot).with_bin_slot_slot(
            Instruction::ADDF as u8,
            10,
            11,
        ));
        il.push_byte(Byte::new(Instruction::CONST).with_const_pool(6));
        il.push_byte(Byte::new(Instruction::GTF));
        il.emit_jump(IlJumpKind::JumpIfFalse, exit);
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(1));
        il.bind_label(exit);
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = vec![0u64; 7];
        pool[6] = 4.0f64.to_bits();
        let lowered = lower(il.ops(), &mut pool);
        assert_ne!(
            *lowered.bytecode[0].bytecode(),
            Instruction::BinSlotSlotConstJmpf,
            "mandelbrot-shaped BinSlotSlotConstJmpf is not fused"
        );
    }

    #[test]
    fn lower_fuses_load_load_addf_const_gtf_jmpf() {
        // Pre-BinSlotSlot IL shape seen in mandelbrot before fuse-select.
        let mut il = IlBuilder::new();
        let exit = il.fresh_label();
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(10));
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(11));
        il.push_byte(Byte::new(Instruction::ADDF));
        il.push_byte(Byte::new(Instruction::CONST).with_const_pool(6));
        il.push_byte(Byte::new(Instruction::GTF));
        il.emit_jump(IlJumpKind::JumpIfFalse, exit);
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(1));
        il.bind_label(exit);
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = vec![0u64; 7];
        pool[6] = 4.0f64.to_bits();
        let lowered = lower(il.ops(), &mut pool);
        assert!(
            !lowered
                .bytecode
                .iter()
                .any(|b| *b.bytecode() == Instruction::BinSlotSlotConstJmpf),
            "LOAD;LOAD;ADDF;CONST;GTF;JMPF must not fuse to BinSlotSlotConstJmpf"
        );
    }

    #[test]
    fn lower_fuses_load_const_bitand_jmpf() {
        let mut il = IlBuilder::new();
        let exit = il.fresh_label();
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(0));
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(1));
        il.push_byte(Byte::new(Instruction::BITAND));
        il.emit_jump(IlJumpKind::JumpIfFalse, exit);
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(1));
        il.bind_label(exit);
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::BinSlotImmJmpf
        ));
        assert_eq!(
            lowered.bytecode[0].bin_slot_imm_jmpf_parts().0,
            Instruction::BITAND as u8
        );
    }

    #[test]
    fn lower_fuses_load_const_add_store_to_bin_slot_imm_store() {
        let mut il = IlBuilder::new();
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(0));
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(1));
        il.push_byte(Byte::new(Instruction::ADD));
        il.push_byte(Byte::new(Instruction::STORE).with_operand_u32(0));
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert_eq!(lowered.bytecode.len(), 2);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::BinSlotImmStore
        ));
        let (op, src, idx) = lowered.bytecode[0].bin_slot_imm_store_parts();
        assert_eq!(op, Instruction::ADD as u8);
        assert_eq!(src, 0);
        let packed = pool[idx];
        assert_eq!(packed >> 32, 0); // dest
        assert_eq!(packed as u16, 1); // imm
    }

    #[test]
    fn lower_fuses_two_stage_float_chain_store() {
        let mut il = IlBuilder::new();
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(1));
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(2));
        il.push_byte(Byte::new(Instruction::SUBF));
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(3));
        il.push_byte(Byte::new(Instruction::ADDF));
        il.push_byte(Byte::new(Instruction::STORE).with_operand_u32(4));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert!(
            !lowered
                .bytecode
                .iter()
                .any(|b| *b.bytecode() == Instruction::FloatChainStore),
            "FloatChainStore is not emitted"
        );
    }

    #[test]
    fn lower_fuses_three_stage_const_under_float_chain_store() {
        // CONST 2.0; BinSlotSlot MULF zr,zi; MULF; LOAD ci; ADDF; STORE zi
        let mut pool = vec![Value::from(2.0_f64).raw() as u64];
        let mut il = IlBuilder::new();
        il.push_byte(Byte::new(Instruction::CONST).with_const_pool(0));
        il.push_byte(Byte::new(Instruction::BinSlotSlot).with_bin_slot_slot(
            Instruction::MULF as u8,
            7,
            8,
        ));
        il.push_byte(Byte::new(Instruction::MULF));
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(6));
        il.push_byte(Byte::new(Instruction::ADDF));
        il.push_byte(Byte::new(Instruction::STORE).with_operand_u32(8));

        let lowered = lower(il.ops(), &mut pool);
        assert!(
            !lowered
                .bytecode
                .iter()
                .any(|b| *b.bytecode() == Instruction::FloatChainStore),
            "FloatChainStore is not emitted"
        );
    }

    #[test]
    fn lower_fuses_bin_slot_slot_two_stage_float_chain_store() {
        let mut il = IlBuilder::new();
        il.push_byte(Byte::new(Instruction::BinSlotSlot).with_bin_slot_slot(
            Instruction::SUBF as u8,
            1,
            2,
        ));
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(3));
        il.push_byte(Byte::new(Instruction::ADDF));
        il.push_byte(Byte::new(Instruction::STORE).with_operand_u32(4));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert!(
            !lowered
                .bytecode
                .iter()
                .any(|b| *b.bytecode() == Instruction::FloatChainStore),
            "FloatChainStore is not emitted"
        );
    }

    /// Stage0 `LOAD; CONST; op` (post-cast_spill shape) fuses with continuations.
    #[test]
    fn lower_fuses_load_const_stage0_float_chain_store() {
        let mut pool = vec![
            Value::from(2.0_f64).raw() as u64,
            Value::from(1.5_f64).raw() as u64,
        ];
        let mut il = IlBuilder::new();
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(10));
        il.push_byte(Byte::new(Instruction::CONST).with_const_pool(0));
        il.push_byte(Byte::new(Instruction::MULF));
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(11));
        il.push_byte(Byte::new(Instruction::DIVF));
        il.push_byte(Byte::new(Instruction::CONST).with_const_pool(1));
        il.push_byte(Byte::new(Instruction::SUBF));
        il.push_byte(Byte::new(Instruction::STORE).with_operand_u32(5));

        let lowered = lower(il.ops(), &mut pool);
        assert!(
            !lowered
                .bytecode
                .iter()
                .any(|b| *b.bytecode() == Instruction::FloatChainStore),
            "FloatChainStore is not emitted"
        );
    }

    /// `CastIntToFloat` spill + const-under stage0 → `FloatChainStore` (mandelbrot `cr`).
    #[test]
    fn cast_spill_feeds_float_chain_store() {
        let loc = DebugLoc::unknown();
        let mut pool = vec![
            Value::from(2.0_f64).raw() as u64,
            Value::from(1.5_f64).raw() as u64,
        ];
        // CONST 2; LOAD x; Cast; LOAD size_f; DIVF; MULF; CONST 1.5; SUBF; STORE
        let ops = vec![
            IlOp::ConstPool { idx: 0, loc },
            IlOp::Load { slot: 4, loc },
            IlOp::byte(Byte::new(Instruction::CastIntToFloat)),
            IlOp::Load { slot: 13, loc },
            IlOp::Bin {
                op: Instruction::DIVF,
                loc,
            },
            IlOp::Bin {
                op: Instruction::MULF,
                loc,
            },
            IlOp::ConstPool { idx: 1, loc },
            IlOp::Bin {
                op: Instruction::SUBF,
                loc,
            },
            IlOp::StorePop { slot: 5, loc },
        ];
        let lowered = lower(&ops, &mut pool);
        assert!(
            !lowered
                .bytecode
                .iter()
                .any(|b| matches!(*b.bytecode(), Instruction::FloatChainStore)),
            "FloatChainStore is not emitted; got {:?}",
            lowered
                .bytecode
                .iter()
                .map(|b| *b.bytecode())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn lower_fuses_load_dup_mul_to_bin_slot_slot() {
        let mut il = IlBuilder::new();
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(7));
        il.push_byte(Byte::new(Instruction::DUPLICATE));
        il.push_byte(Byte::new(Instruction::MUL));
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert!(
            matches!(*lowered.bytecode[0].bytecode(), Instruction::BinSlotSlot),
            "LOAD; DUP; MUL should fuse; got {:?}",
            lowered.bytecode[0].bytecode()
        );
        let (op, a, b) = lowered.bytecode[0].bin_slot_slot_parts();
        assert_eq!(op, Instruction::MUL as u8);
        assert_eq!(a, 7);
        assert_eq!(b, 7);
    }

    #[test]
    fn lower_fuses_load_load_and_store_to_bin_slot_slot_store() {
        let mut il = IlBuilder::new();
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(0));
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(1));
        il.push_byte(Byte::new(Instruction::AND));
        il.push_byte(Byte::new(Instruction::STORE).with_operand_u32(2));
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert_eq!(lowered.bytecode.len(), 2);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::BinSlotSlotStore
        ));
        assert_eq!(
            lowered.bytecode[0].bin_slot_slot_store_parts(),
            (Instruction::AND as u8, 0, 1, 2)
        );
    }

    #[test]
    fn lower_fuses_bin_slot_imm_then_store() {
        let mut il = IlBuilder::new();
        il.push_byte(Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(
            Instruction::BITAND as u8,
            0,
            7,
        ));
        il.push_byte(Byte::new(Instruction::STORE).with_operand_u32(1));
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::BinSlotImmStore
        ));
        let (op, src, idx) = lowered.bytecode[0].bin_slot_imm_store_parts();
        assert_eq!(op, Instruction::BITAND as u8);
        assert_eq!(src, 0);
        assert_eq!(pool[idx] >> 32, 1);
        assert_eq!(pool[idx] as u16, 7);
    }

    #[test]
    fn lower_packs_three_adjacent_loads() {
        let mut il = IlBuilder::new();
        il.push_byte(Byte::new(Instruction::LOAD).with_load_store_slot(0));
        il.push_byte(Byte::new(Instruction::LOAD).with_load_store_slot(1));
        il.push_byte(Byte::new(Instruction::LOAD).with_load_store_slot(2));
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert_eq!(lowered.bytecode.len(), 2);
        assert!(matches!(*lowered.bytecode[0].bytecode(), Instruction::LOAD));
        assert_eq!(lowered.bytecode[0].load_store_parts(), (3, 0, 1, 2));
        assert_eq!(lowered.bytecode[0].load_store_count(), 3);
    }

    #[test]
    fn lower_packs_two_adjacent_stores_ordering() {
        // Consecutive STORE s0; STORE s1 pops TOS→s0 then next→s1.
        let mut il = IlBuilder::new();
        il.push_byte(Byte::new(Instruction::STORE).with_load_store_slot(0));
        il.push_byte(Byte::new(Instruction::STORE).with_load_store_slot(1));
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert_eq!(lowered.bytecode.len(), 2);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::STORE
        ));
        assert_eq!(lowered.bytecode[0].load_store_parts(), (2, 0, 1, 0));
        assert_eq!(lowered.bytecode[0].load_store_slot_at(0), 0);
        assert_eq!(lowered.bytecode[0].load_store_slot_at(1), 1);
    }

    #[test]
    fn lower_refuses_packed_loads_when_label_mid_window() {
        let mut il = IlBuilder::new();
        let mid = il.fresh_label();
        il.push_byte(Byte::new(Instruction::LOAD).with_load_store_slot(0));
        il.bind_label(mid);
        il.push_byte(Byte::new(Instruction::LOAD).with_load_store_slot(1));
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert_eq!(lowered.bytecode.len(), 3);
        assert_eq!(lowered.bytecode[0].load_store_single_slot(), Some(0));
        assert_eq!(lowered.bytecode[1].load_store_single_slot(), Some(1));
        assert_eq!(lowered.label_pcs.get(&mid.0).copied(), Some(1));
    }

    /// Unconditional JMP-to-join must not land on ConstReturnImm: stacked
    /// arm value is ignored. Compare-jumps to the same join may fuse.
    #[test]
    fn lower_refuses_const_return_fuse_when_uncond_jmp_binds_producer() {
        let mut il = IlBuilder::new();
        let join = il.fresh_label();
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(1));
        il.emit_jump(IlJumpKind::Unconditional, join);
        il.bind_label(join);
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(7));
        il.push_byte(Byte::new(Instruction::RETURN));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert!(
            lowered
                .bytecode
                .iter()
                .any(|b| matches!(*b.bytecode(), Instruction::CONST) && b.operand_u32() == 7),
            "uncond JMP join must keep CONST; RETURN unfused; got {:?}",
            lowered
                .bytecode
                .iter()
                .map(|b| *b.bytecode())
                .collect::<Vec<_>>()
        );
        assert!(
            !lowered
                .bytecode
                .iter()
                .any(|b| matches!(*b.bytecode(), Instruction::ConstReturnImm)),
            "must not fuse ConstReturnImm on an uncond JMP join"
        );
    }

    #[test]
    fn lower_fuses_const_return_when_only_cond_jump_targets_producer() {
        let mut il = IlBuilder::new();
        let base = il.fresh_label();
        il.push_byte(Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(
            Instruction::LEQ as u8,
            0,
            2,
        ));
        il.emit_jump(IlJumpKind::JumpIfTrue, base);
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(0));
        il.push_byte(Byte::new(Instruction::RETURN));
        il.bind_label(base);
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(1));
        il.push_byte(Byte::new(Instruction::RETURN));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert!(
            lowered
                .bytecode
                .iter()
                .any(|b| matches!(*b.bytecode(), Instruction::ConstReturnImm)
                    && b.operand_u32() == 1),
            "cond-jump to `return 1` should fuse ConstReturnImm; got {:?}",
            lowered
                .bytecode
                .iter()
                .map(|b| *b.bytecode())
                .collect::<Vec<_>>()
        );
    }

    /// A mid-window label bind is a fuse barrier (match joins / attr sites).
    #[test]
    fn lower_refuses_bin_slot_slot_when_label_mid_window() {
        let mut il = IlBuilder::new();
        let mid = il.fresh_label();
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(0));
        il.bind_label(mid);
        il.push_byte(Byte::new(Instruction::LOAD).with_operand_u32(1));
        il.push_byte(Byte::new(Instruction::ADD));
        il.push_byte(Byte::new(Instruction::RETURN));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert!(
            !lowered
                .bytecode
                .iter()
                .any(|b| matches!(*b.bytecode(), Instruction::BinSlotSlot)),
            "label mid-window must block BinSlotSlot fuse; got {:?}",
            lowered
                .bytecode
                .iter()
                .map(|b| b.bytecode())
                .collect::<Vec<_>>()
        );
        assert!(matches!(*lowered.bytecode[0].bytecode(), Instruction::LOAD));
        assert_eq!(lowered.label_pcs.get(&mid.0).copied(), Some(1));
    }

    #[test]
    fn lower_resolves_jump_if_match_into_pool() {
        let mut il = IlBuilder::new();
        let arm = il.fresh_label();
        il.emit_jump(IlJumpKind::JumpIfMatch { tag: 2, arity: 1 }, arm);
        il.push_byte(Byte::new(Instruction::CONST).with_const_inline(0));
        il.bind_label(arm);
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::JumpIfMatch
        ));
        assert_eq!(lowered.bytecode[0].operand_u16(0), 2);
        let idx = lowered.bytecode[0].operand_u16(1);
        assert_eq!(pool[idx as usize], 2); // HALT at PC 2
    }

    #[test]
    fn lower_resolves_entry_call() {
        let mut il = IlBuilder::new();
        let entry = il.fresh_label();
        il.emit_entry(EntryKind::Call, 1, entry);
        il.push_byte(Byte::new(Instruction::HALT));
        il.bind_label(entry);
        il.push_byte(Byte::new(Instruction::RETURN));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert!(matches!(*lowered.bytecode[0].bytecode(), Instruction::CALL));
        assert_eq!(lowered.bytecode[0].call_parts(), (1, 2));
    }

    #[test]
    fn lower_resolves_entry_tail_call_and_code_ptr() {
        let mut il = IlBuilder::new();
        let entry = il.fresh_label();
        il.emit_entry(EntryKind::TailCall, 2, entry);
        il.emit_entry(EntryKind::CodePtr, 0, entry);
        il.push_byte(Byte::new(Instruction::HALT));
        il.bind_label(entry);
        il.push_byte(Byte::new(Instruction::RETURN));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::TailCall
        ));
        assert_eq!(lowered.bytecode[0].call_parts(), (2, 3));
        assert!(matches!(
            *lowered.bytecode[1].bytecode(),
            Instruction::CodePtr
        ));
        assert_eq!(lowered.bytecode[1].operand_u32(), 3);
    }

    #[test]
    fn residual_abs_jmp_byte_is_detected() {
        let ops = vec![IlOp::byte(Byte::new(Instruction::JMP).with_operand_u32(42))];
        assert!(is_residual_abs_jump(&ops[0]));
    }

    #[test]
    fn prologue_sentinel_jmp_is_not_residual() {
        let ops = vec![
            IlOp::PrologueJmp {
                loc: DebugLoc::unknown(),
            },
            IlOp::byte(Byte::new(Instruction::JMP).with_operand_u32(u32::MAX)),
        ];
        assert!(!is_residual_abs_jump(&ops[0]));
        assert!(!is_residual_abs_jump(&ops[1]));
        assert_no_residual_abs_jumps(&ops);
    }

    #[test]
    #[should_panic(expected = "residual abs JMP")]
    fn lower_panics_on_residual_abs_jmp_byte() {
        let ops = vec![
            IlOp::byte(Byte::new(Instruction::JMP).with_operand_u32(1)),
            IlOp::byte(Byte::new(Instruction::HALT)),
        ];
        let mut pool = Vec::new();
        let _ = lower(&ops, &mut pool);
    }

    #[test]
    fn fuse_select_refuses_residual_byte_in_window() {
        // FORMAT is the documented cold set; it must not join a Load/Const/Bin fuse.
        let ops = vec![
            IlOp::Load {
                slot: 0,
                loc: DebugLoc::unknown(),
            },
            IlOp::Byte {
                byte: Byte::new(Instruction::FORMAT),
                loc: DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 1,
                loc: DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: DebugLoc::unknown(), ret_words: 1,},
        ];
        let mut pool = Vec::new();
        let lowered = lower(&ops, &mut pool);
        assert!(
            lowered
                .bytecode
                .iter()
                .any(|b| *b.bytecode() == Instruction::FORMAT),
            "cold Byte must survive encode"
        );
        assert!(
            lowered
                .bytecode
                .iter()
                .any(|b| *b.bytecode() == Instruction::LOAD),
            "Load before FORMAT must not be swallowed into BinSlotImm"
        );
        assert!(
            !lowered
                .bytecode
                .iter()
                .any(|b| *b.bytecode() == Instruction::BinSlotImm
                    || *b.bytecode() == Instruction::BinSlotSlot),
            "fuse must refuse a window that contains residual Byte"
        );
    }

    /// Typed hot-set ops must encode through lower's `as_encode_byte` path and still fuse.
    #[test]
    fn lower_fuses_typed_load_const_bin_ops() {
        let ops = vec![
            IlOp::Load {
                slot: 0,
                loc: DebugLoc::unknown(),
            },
            IlOp::Load {
                slot: 1,
                loc: DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: DebugLoc::unknown(), ret_words: 1,},
        ];
        let mut pool = Vec::new();
        let lowered = lower(&ops, &mut pool);
        assert_eq!(lowered.bytecode.len(), 2);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::BinSlotSlot
        ));
        assert!(matches!(
            *lowered.bytecode[1].bytecode(),
            Instruction::RETURN
        ));
    }

    #[test]
    fn lower_fuses_typed_const_return() {
        let ops = vec![
            IlOp::Const {
                imm: 11,
                loc: DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: DebugLoc::unknown(), ret_words: 1,},
        ];
        let mut pool = Vec::new();
        let lowered = lower(&ops, &mut pool);
        assert_eq!(lowered.bytecode.len(), 1);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::ConstReturnImm
        ));
        assert_eq!(lowered.bytecode[0].operand_u32(), 11);
    }

    /// `lower_with_funcs` must rebuild an owning module, scope body opts, then lower once.
    #[test]
    fn lower_with_funcs_scopes_body_opts_then_lowers_once() {
        let loc = DebugLoc::unknown();
        let ops = vec![
            IlOp::Dup { loc },
            IlOp::Pop { loc },
            IlOp::Const { imm: 5, loc },
            IlOp::Return { loc, ret_words: 1},
            IlOp::Dup { loc },
            IlOp::Pop { loc },
        ];
        let funcs = vec![IlFunc::new("f", None, 2, 4)];
        let mut pool = Vec::new();
        let lowered = lower_with_funcs(&ops, &funcs, &mut pool);
        assert!(
            matches!(*lowered.bytecode[0].bytecode(), Instruction::DUPLICATE),
            "prologue must survive scoped opts"
        );
        assert!(matches!(*lowered.bytecode[1].bytecode(), Instruction::POP));
        assert!(
            matches!(*lowered.bytecode[2].bytecode(), Instruction::ConstReturnImm),
            "body Const+Return must fuse in the single lower"
        );
        assert_eq!(lowered.bytecode[2].operand_u32(), 5);
        assert!(
            matches!(*lowered.bytecode[3].bytecode(), Instruction::DUPLICATE),
            "trailing glue must survive"
        );
        assert!(matches!(*lowered.bytecode[4].bytecode(), Instruction::POP));
    }

    /// `lower_module` must run CFG GVN on bodies before the single fuse/PC lower.
    #[test]
    fn lower_module_runs_gvn_load_join_cse_before_lower() {
        let loc = DebugLoc::unknown();
        let ops = vec![
            IlOp::Load { slot: 1, loc },
            IlOp::Const { imm: 99, loc },
            IlOp::Bin {
                op: Instruction::EQ,
                loc,
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc,
                hint: Default::default(),
            },
            IlOp::Load { slot: 3, loc },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc,
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Load { slot: 3, loc },
            IlOp::Label(Label(2)),
            IlOp::Load { slot: 3, loc },
            IlOp::Return { loc, ret_words: 1},
        ];
        let emit_end = ops.iter().filter(|op| op.emits_code()).count();
        let funcs = vec![IlFunc::new("f", None, 0, emit_end)];
        let mut module = super::super::IlModule::from_flat(&ops, &funcs);
        let mut pool = Vec::new();
        let lowered = lower_module(&mut module, &mut pool);
        let loads = lowered
            .bytecode
            .iter()
            .filter(|b| matches!(*b.bytecode(), Instruction::LOAD))
            .count();
        assert_eq!(
            loads,
            2,
            "join Load CSE via lower_module GVN; got {:?}",
            lowered
                .bytecode
                .iter()
                .map(|b| *b.bytecode())
                .collect::<Vec<_>>()
        );
        assert!(
            lowered
                .bytecode
                .iter()
                .any(|b| matches!(*b.bytecode(), Instruction::RETURN)),
            "stack value from pred Load must still return"
        );
    }

    /// Operand-order canon turns `Const; Load; ADD` into `Load; Const; ADD`,
    /// which fuse-select encodes as `BinSlotImm`.
    #[test]
    fn canon_feeds_bin_slot_imm_fuse_shape() {
        let loc = DebugLoc::unknown();
        let ops = vec![
            IlOp::Const { imm: 1, loc },
            IlOp::Load { slot: 0, loc },
            IlOp::Bin {
                op: Instruction::ADD,
                loc,
            },
            IlOp::Return { loc, ret_words: 1},
        ];
        let mut pool = Vec::new();
        let lowered = lower(&ops, &mut pool);
        assert!(
            lowered
                .bytecode
                .iter()
                .any(|b| matches!(*b.bytecode(), Instruction::BinSlotImm)),
            "expected BinSlotImm after canon+fuse; got {:?}",
            lowered
                .bytecode
                .iter()
                .map(|b| *b.bytecode())
                .collect::<Vec<_>>()
        );
    }

    /// Int `ConstPool; Load; ADD` demotes then fuses to `BinSlotImm`.
    #[test]
    fn canon_demotes_const_pool_then_fuses_bin_slot_imm() {
        let loc = DebugLoc::unknown();
        let ops = vec![
            IlOp::ConstPool { idx: 0, loc },
            IlOp::Load { slot: 0, loc },
            IlOp::Bin {
                op: Instruction::ADD,
                loc,
            },
            IlOp::Return { loc, ret_words: 1},
        ];
        let mut pool = vec![common::Value::from(3_i64).raw() as u64];
        let lowered = lower(&ops, &mut pool);
        assert!(
            lowered
                .bytecode
                .iter()
                .any(|b| matches!(*b.bytecode(), Instruction::BinSlotImm)),
            "expected BinSlotImm after ConstPool demote+canon+fuse; got {:?}",
            lowered
                .bytecode
                .iter()
                .map(|b| *b.bytecode())
                .collect::<Vec<_>>()
        );
    }

    /// COI-80 retain path: only `capture_ops` fills `pre_fuse_ops` for the gate.
    #[test]
    fn lower_module_inner_captures_pre_fuse_ops_only_when_requested() {
        let loc = DebugLoc::unknown();
        let ops = vec![IlOp::Const { imm: 1, loc }, IlOp::Return { loc, ret_words: 1}];
        let mut pool = Vec::new();
        let mut module = crate::il::IlModule::from_flat(&ops, &[]);
        let plain = lower_module_inner(
            &mut module,
            &mut pool,
            false,
            &crate::il::opt::OptimizeOptions::default(),
        )
        .expect("well-formed IL");
        assert!(plain.pre_fuse_ops.is_none());

        let mut module = crate::il::IlModule::from_flat(&ops, &[]);
        let captured = lower_module_inner(
            &mut module,
            &mut pool,
            true,
            &crate::il::opt::OptimizeOptions::default(),
        )
        .expect("well-formed IL");
        let snap = captured
            .pre_fuse_ops
            .as_ref()
            .expect("capture_ops must retain post-opt flat");
        assert!(!snap.is_empty());
        let emitting = snap.iter().filter(|op| op.emits_code()).count();
        assert_eq!(
            captured.pre_to_post.len(),
            emitting,
            "pre_to_post keys cover every emitting pre-fuse op"
        );
    }
}
