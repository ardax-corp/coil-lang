//! Lower verified MIR to stack IL (LIR) using the shipped Value / two-slot ABI.
//!
//! Dense opcodes stay on the numeric path. This emit uses `LOAD`/`STORE`/`Bin`
//! and `RETURN` width 1 or 2 — no new pair opcodes.
//!
//! Single-use values (return words, cmp immediates) stay on the stack instead
//! of `STORE`+`LOAD`. That is the quality gap vs naive SSA slot reconstruct.

use std::collections::HashMap;

use common::{Byte, DebugLoc, Instruction};

use crate::il::{IlJumpKind, IlOp, Label};

use super::emit::{
    coalesce_latch_overwrite, emit_cond_jumps, foreign_targets, il_for_alloc, is_fallthrough,
    max_label_hint, paired_alloc_dest, restore_block_op_locs, take_label, term_cmp_dest,
};
use super::func::MirFunc;
use super::inst::{
    BlockId, MirAllocKind, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, MirUnaryOp,
    Terminator, ValueId,
};
use super::layout::MirLayout;
use super::error::LowerError;
use super::ty::MirTy;

/// Emit fuse-IL for `func`. Preserves `entry_label` so CALL targets stay valid.
///
/// `across_alloc` is S2c: reconstruct `Make*` / `InitTyped` when maps exist.
pub fn emit_lir(
    func: &MirFunc,
    entry_label: Option<Label>,
    pool: &mut Vec<u64>,
    across_alloc: bool,
) -> Result<Vec<IlOp>, LowerError> {
    emit_lir_with_aliases(func, entry_label, &[], pool, across_alloc)
}

/// [`emit_lir`] for a body whose entry is also bound under `aliases` (the
/// other labels leading the source body): they stay bound at the entry, so
/// a self call through one of them still lands there.
pub fn emit_lir_with_aliases(
    func: &MirFunc,
    entry_label: Option<Label>,
    aliases: &[Label],
    pool: &mut Vec<u64>,
    across_alloc: bool,
) -> Result<Vec<IlOp>, LowerError> {
    if func.has_mapped_gc_edge() && !across_alloc {
        return Err(LowerError::Refused(
            "MIR→LIR refuses Alloc/GcBarrier without S2b maps (S2c)".into(),
        ));
    }
    let mut plan = EmitPlan::new(func);
    let (regs, scratch) = assign_needed(func, &plan)?;
    plan.home_base = home_base(func, scratch)?;
    let plan = plan;
    let regs = coalesce_latch_overwrite(func, regs, &plan.need_slot);
    let cleared = super::emit::cleared_words(func, &regs, &plan.need_slot);
    let max_reg = plan
        .need_slot
        .iter()
        .enumerate()
        .filter(|(_, n)| **n)
        .map(|(i, _)| regs[i])
        .max()
        .unwrap_or(0);
    // As `emit_mir`: block labels skip the entries this body calls, and the
    // entry's aliases, which bind at the entry (a block that took one would
    // bind it a second time).
    let mut reserved = foreign_targets(func, entry_label);
    reserved.extend(aliases.iter().map(|alias| alias.0));
    let mut next_label = max_label_hint(entry_label);
    let mut block_lab = vec![Label(0); func.blocks.len()];
    for b in &func.blocks {
        if b.id == func.entry {
            block_lab[b.id.index()] = entry_label.unwrap_or_else(|| take_label(&mut next_label, &reserved));
        } else {
            block_lab[b.id.index()] = take_label(&mut next_label, &reserved);
        }
    }

    let mut out = Vec::new();
    out.push(IlOp::Label(block_lab[func.entry.index()]));
    out.extend(aliases.iter().map(|&l| IlOp::Label(l)));
    let mut frame = u32::from(max_reg) + 1;
    if !func.homes.is_empty() {
        frame = frame.max(plan.home_slot(0) + func.homes.len() as u32);
    }
    if frame > func.params.len() as u32 {
        out.push(IlOp::byte(
            Byte::new(Instruction::Seek).with_operand_u32(frame),
        ));
    }

    for block in &func.blocks {
        if block.id != func.entry {
            out.push(IlOp::Label(block_lab[block.id.index()]));
        }
        let term_loc = func.term_loc(block.id);
        consume_match_tos(&mut out, func, block.id, &plan, &regs, term_loc);
        // A miss leaves the scrutinee on the stack: the next arm's test
        // peeks it there; a block taking no payload of it pops it.
        if super::emit::miss_chain_tos(func, block.id).is_none() && super::emit::missed_scrutinee_unused(func, block.id) {
            out.push(IlOp::Pop { loc: term_loc });
        }
        let mut starts: Vec<(usize, ValueId)> = Vec::new();
        for inst in &block.insts {
            if inst.is_phi() {
                continue;
            }
            starts.push((out.len(), inst.dest()));
            let loc = func.loc_of(inst.dest());
            if let MirInst::MatchPayload { dest, scrutinee, index } = inst {
                if is_jim_term_payload(func, *dest) {
                    continue;
                }
                if *index == 0 {
                    emit_last_arm_unpack(EmitLastArmUnpackArgs {
                        out: &mut out,
                        func,
                        block: block.id,
                        scrutinee: *scrutinee,
                        plan: &plan,
                        regs: &regs,
                        pool,
                        loc,
                    })?;
                }
                continue;
            }
            if let MirInst::ClearWord { dest, value } = inst {
                if cleared.contains(dest) {
                    super::emit::push_clear_word(&mut out, regs[value.index()]);
                }
                continue;
            }
            if let MirInst::KeepAlive { .. } = inst {
                continue;
            }
            let dest = inst.dest();
            if plan.tree[dest.index()] {
                continue;
            }
            // A tail call is the block's `Return` (`emit_term`).
            if tail_call(func, block).is_some()
                && let MirInst::Call { dest: d, .. } = inst
                && *d == dest
            {
                continue;
            }
            if term_cmp_dest(func, block).is_some_and(|d| dest == d) {
                continue;
            }
            emit_stored(&mut out, inst, func, &plan, &regs, pool, loc)?;
        }
        let end = out.len();
        restore_block_op_locs(&mut out, &starts, end, func);
        emit_term(EmitTermArgs {
            out: &mut out,
            block,
            func,
            plan: &plan,
            regs: &regs,
            scratch,
            block_lab: &block_lab,
            next_label: &mut next_label,
            reserved: &reserved,
            pool,
            loc: term_loc,
        })?;
    }
    Ok(out)
}

pub(super) type LirSidecars = (
    std::collections::HashMap<u32, u32>,
    super::deopt::DraftDeoptMap,
    Option<super::stackmap::DraftFrameMap>,
);

/// Debug slot remap, deopt map and (for a body with GC edges) the stack map
/// of `func`, in the frame registers LIR emission assigns.
pub(super) fn lir_sidecars(func: &MirFunc) -> LirSidecars {
    let plan = EmitPlan::new(func);
    let (regs, scratch) = assign_needed(func, &plan).unwrap_or_else(|_| (Vec::new(), 0));
    let regs = coalesce_latch_overwrite(func, regs, &plan.need_slot);
    let mut stack_map = if func.has_mapped_gc_edge() && !regs.is_empty() {
        super::stackmap::encode_draft_regs(func, &regs, &plan.need_slot)
    } else {
        None
    };
    // A heap home holds its `defer` capture to the end of the body, past
    // the capture's own last use: every site roots it.
    if let (Some(map), Ok(base)) = (stack_map.as_mut(), home_base(func, scratch)) {
        let heap = func.homes.iter().enumerate().filter(|(_, ty)| ty.is_heap_word());
        for (k, _) in heap {
            let slot = u16::from(base) + k as u16;
            for site in &mut map.sites {
                if !site.contains(&slot) {
                    site.push(slot);
                    site.sort_unstable();
                }
            }
            if !map.frame_slots.contains(&slot) {
                map.frame_slots.push(slot);
                map.frame_slots.sort_unstable();
            }
        }
    }
    (
        super::deopt::debug_slot_remap(func, &regs, &plan.need_slot),
        super::deopt::encode_draft(func, &regs, &plan.need_slot),
        stack_map,
    )
}

/// The frame `func`'s LIR keeps its homes past: `(home 0's word, frame
/// words)`, `None` when emission would refuse it.
pub(super) fn lir_home_frame(func: &MirFunc) -> Option<(u8, u32)> {
    let mut plan = EmitPlan::new(func);
    let (regs, scratch) = assign_needed(func, &plan).ok()?;
    plan.home_base = home_base(func, scratch).ok()?;
    let regs = coalesce_latch_overwrite(func, regs, &plan.need_slot);
    let max_reg = plan.need_slot.iter().enumerate().filter(|(_, n)| **n).map(|(i, _)| regs[i]).max().unwrap_or(0);
    let frame = (u32::from(max_reg) + 1).max(plan.home_slot(0) + func.homes.len() as u32);
    Some((plan.home_base, frame))
}

struct EmitPlan {
    tree: Vec<bool>,
    need_slot: Vec<bool>,
    def: Vec<Option<(BlockId, usize)>>,
    /// Frame word of home 0 ([`MirFunc::homes`]).
    home_base: u8,
}

impl EmitPlan {
    fn new(func: &MirFunc) -> Self {
        let n = func.types.len();
        let mut uses = vec![0u32; n];
        let mut phi_in = vec![false; n];
        let mut def = vec![None; n];
        let mut def_block = vec![None; n];

        for (i, p) in func.params.iter().enumerate() {
            def_block[p.index()] = Some(func.entry);
            let _ = i;
        }
        for block in &func.blocks {
            for (i, inst) in block.insts.iter().enumerate() {
                for d in inst.dests() {
                    def[d.index()] = Some((block.id, i));
                    def_block[d.index()] = Some(block.id);
                }
                if inst.is_phi() {
                    // A φ dest is written by the edge moves, so it lives in
                    // its slot: never an inline (tree) operand.
                    phi_in[inst.dest().index()] = true;
                    for v in inst.operands() {
                        phi_in[v.index()] = true;
                    }
                }
                // A clear reads no word: a value it alone follows can
                // still be an inline operand (then it has no register).
                if matches!(inst, MirInst::ClearWord { .. }) {
                    continue;
                }
                for v in inst.operands() {
                    uses[v.index()] += 1;
                }
            }
            if let Some(term) = &block.term {
                for v in term_values(term) {
                    uses[v.index()] += 1;
                }
            }
        }

        // Under a debugger each named local's value keeps its own word,
        // as a φ's does: the debugger reads it there.
        if func.debug_keep {
            for v in func.debug_slots.values() {
                if let Some(pin) = phi_in.get_mut(v.index()) {
                    *pin = true;
                }
            }
        }
        let fused = fused_cmp_dests(func);
        let mut tree = vec![false; n];
        let mut changed = true;
        while changed {
            changed = false;
            for block in &func.blocks {
                if let Some(Terminator::Return { lo, hi }) = &block.term {
                    for v in [*lo, *hi].into_iter().flatten() {
                        changed |= mark_tree(MarkTreeArgs {
                            v,
                            use_block: block.id,
                            tree: &mut tree,
                            uses: &uses,
                            phi_in: &phi_in,
                            def_block: &def_block,
                            params: &func.params,
                            fused: &fused,
                        });
                    }
                }
                if let Some(d) = term_cmp_dest(func, block)
                    && let Some(MirInst::Cmp { lhs, rhs, .. }) =
                        def[d.index()].and_then(|(b, i)| {
                            (b == block.id).then_some(&func.block(b).insts[i])
                        })
                    {
                        for v in [*lhs, *rhs] {
                            changed |= mark_tree(MarkTreeArgs {
                                v,
                                use_block: block.id,
                                tree: &mut tree,
                                uses: &uses,
                                phi_in: &phi_in,
                                def_block: &def_block,
                                params: &func.params,
                                fused: &fused,
                            });
                        }
                    }
                for inst in &block.insts {
                    // A pinned word stays in its slot: the pin emits nothing
                    // that could take it as an inline operand.
                    if inst.is_phi() || fused[inst.dest().index()] || matches!(inst, MirInst::KeepAlive { .. }) {
                        continue;
                    }
                    // Operands of stored bins can still be tree (i % 10 → BinSlotImm).
                    for v in inst.operands() {
                        changed |= mark_tree(MarkTreeArgs {
                            v,
                            use_block: block.id,
                            tree: &mut tree,
                            uses: &uses,
                            phi_in: &phi_in,
                            def_block: &def_block,
                            params: &func.params,
                            fused: &fused,
                        });
                    }
                }
            }
        }

        keep_memory_order(func, &mut tree, &fused);
        keep_payload_first(func, &mut tree, &fused);

        let mut need_slot = vec![false; n];
        for p in &func.params {
            need_slot[p.index()] = true;
        }
        for i in 0..n {
            if fused[i] || tree[i] {
                continue;
            }
            if def[i].is_some() || def_block[i].is_some() {
                let print_tok = def[i].is_some_and(|(b, k)| {
                    matches!(func.block(b).insts.get(k), Some(MirInst::Print { .. } | MirInst::Panic { .. } | MirInst::StoreStatic { .. } | MirInst::HomeStore { .. } | MirInst::ClearWord { .. } | MirInst::KeepAlive { .. }))
                });
                if !print_tok {
                    need_slot[i] = true;
                }
            }
        }
        for block in &func.blocks {
            for inst in &block.insts {
                if let MirInst::Call {
                    dest,
                    dest_hi: Some(hi),
                    ..
                } = inst
                {
                    need_slot[dest.index()] = true;
                    need_slot[hi.index()] = true;
                    tree[dest.index()] = false;
                    tree[hi.index()] = false;
                }
                // Host calls and yields have effects: emit them in program
                // order and keep the result in a slot, never as an inline
                // operand (a yield's sent word arrives only for a store).
                if let MirInst::HostInvoke { dest, .. } | MirInst::Ffi { dest, .. } | MirInst::CallIndirect { dest, .. } | MirInst::Yield { dest, .. } = inst {
                    need_slot[dest.index()] = true;
                    tree[dest.index()] = false;
                }
            }
        }
        for block in &func.blocks {
            let mut n_by_src: Vec<(ValueId, u32)> = Vec::new();
            for inst in &block.insts {
                if let MirInst::MatchPayload { scrutinee, .. } = inst {
                    if let Some((_, n)) = n_by_src.iter_mut().find(|(s, _)| *s == *scrutinee) {
                        *n += 1;
                    } else {
                        n_by_src.push((*scrutinee, 1));
                    }
                }
            }
            for inst in &block.insts {
                let MirInst::MatchPayload { dest, scrutinee, .. } = inst else {
                    continue;
                };
                if n_by_src.iter().any(|(s, n)| *s == *scrutinee && *n > 1) {
                    need_slot[dest.index()] = true;
                    tree[dest.index()] = false;
                }
            }
        }

        Self {
            tree,
            need_slot,
            def,
            home_base: 0,
        }
    }

    /// The frame word of `home`.
    fn home_slot(&self, home: u16) -> u32 {
        u32::from(self.home_base) + u32::from(home)
    }
}

/// Frame word of `func`'s home 0: its cleanup pad's function's, or past
/// every register and the phi-cycle `scratch` word.
fn home_base(func: &MirFunc, scratch: u8) -> Result<u8, LowerError> {
    let base = func.home_base.unwrap_or(scratch.saturating_add(1));
    if usize::from(base) + func.homes.len() > 255 {
        return Err(LowerError::Refused("too many homes".into()));
    }
    Ok(base)
}

struct MarkTreeArgs<'args> {
    v: ValueId,
    use_block: BlockId,
    tree: &'args mut [bool],
    uses: &'args [u32],
    phi_in: &'args [bool],
    def_block: &'args [Option<BlockId>],
    params: &'args [ValueId],
    fused: &'args [bool],
}

fn mark_tree(args: MarkTreeArgs<'_>) -> bool {
    let MarkTreeArgs {
        v,
        use_block,
        tree,
        uses,
        phi_in,
        def_block,
        params,
        fused,
    } = args;

    let i = v.index();
    if tree[i] || fused[i] || phi_in[i] || uses[i] != 1 {
        return false;
    }
    if params.iter().any(|p| p.index() == i) {
        return false;
    }
    if def_block[i] != Some(use_block) {
        return false;
    }
    tree[i] = true;
    true
}

/// A write or call: emitted where it stands, never as an inline operand.
fn writes_memory(inst: &MirInst) -> bool {
    matches!(
        inst,
        MirInst::HeapFieldStore { .. }
            | MirInst::FieldStore { .. }
            | MirInst::StoreIndex { .. }
            | MirInst::ArrayPush { .. }
            | MirInst::Call { .. }
            | MirInst::CallIndirect { .. }
            | MirInst::HostInvoke { .. }
            | MirInst::Ffi { .. }
            | MirInst::ResumeCoro { .. }
            | MirInst::Yield { .. }
            | MirInst::StoreStatic { .. }
            | MirInst::HomeStore { .. }
            | MirInst::ClearWord { .. }
            | MirInst::Print { .. }
            | MirInst::Panic { .. }
    )
}

fn reads_memory(inst: &MirInst) -> bool {
    matches!(
        inst,
        MirInst::HeapFieldLoad { .. } | MirInst::FieldLoad { .. } | MirInst::Index { .. } | MirInst::ArrayLen { .. } | MirInst::DoneCoro { .. } | MirInst::LoadStatic { .. } | MirInst::HomeLoad { .. }
    )
}

/// An inline operand is emitted where its user is, which may reorder it
/// past a memory op between: a read past a write (`let s = o.f; o.f = x;
/// return s;`) or a write past either. Such an operand keeps its slot.
fn keep_memory_order(func: &MirFunc, tree: &mut [bool], fused: &[bool]) {
    for block in &func.blocks {
        let insts = &block.insts;
        let at: HashMap<ValueId, usize> = insts.iter().enumerate().map(|(k, i)| (i.dest(), k)).collect();
        loop {
            let inline = |v: ValueId, tree: &[bool]| tree[v.index()] || fused[v.index()];
            // Instruction indices in the order emission reaches them.
            let mut order = Vec::with_capacity(insts.len());
            fn visit(v: ValueId, at: &HashMap<ValueId, usize>, insts: &[MirInst], inline: &dyn Fn(ValueId) -> bool, order: &mut Vec<usize>) {
                let Some(&k) = at.get(&v) else { return };
                if !inline(v) {
                    return;
                }
                for op in insts[k].operands() {
                    visit(op, at, insts, inline, order);
                }
                order.push(k);
            }
            let is_inline = |v: ValueId| inline(v, tree);
            for (k, inst) in insts.iter().enumerate() {
                if inst.is_phi() || is_inline(inst.dest()) {
                    continue;
                }
                for op in inst.operands() {
                    visit(op, &at, insts, &is_inline, &mut order);
                }
                order.push(k);
            }
            if let Some(term) = &block.term {
                for v in term_values(term) {
                    visit(v, &at, insts, &is_inline, &mut order);
                }
            }
            // A clear zeroes its value's slot where it stands, so an
            // inline user from before the clear must not be emitted after
            // it (`let j = join(s); clear s; return j` would read the
            // zeroed slot): that user keeps its slot.
            let stale = order.iter().enumerate().find_map(|(i, &c)| {
                let MirInst::ClearWord { value, .. } = &insts[c] else {
                    return None;
                };
                order[i + 1..]
                    .iter()
                    .find(|&&k| k < c && insts[k].operands().contains(value))
                    .map(|&k| insts[k].dest())
            });
            if let Some(v) = stale
                && tree[v.index()]
            {
                tree[v.index()] = false;
                continue;
            }
            let mem: Vec<usize> = order.into_iter().filter(|&k| reads_memory(&insts[k]) || writes_memory(&insts[k])).collect();
            // A later-defined op emitted first means an earlier one was
            // delayed past it.
            let late = mem.iter().enumerate().find_map(|(i, &a)| {
                mem[i + 1..]
                    .iter()
                    .find(|&&b| b < a && (writes_memory(&insts[a]) || writes_memory(&insts[b])))
                    .map(|&b| insts[b].dest())
            });
            match late {
                Some(v) if tree[v.index()] => tree[v.index()] = false,
                _ => break,
            }
        }
    }
}

/// A last-arm payload `Unpack` leaves on the stack is an inline operand
/// that emits nothing: its user finds it on top only when it is the first
/// word that user pushes (`format(s, show(p))` pushes `s` first, so the
/// call would take `s`). Otherwise the payload keeps its slot.
fn keep_payload_first(func: &MirFunc, tree: &mut [bool], fused: &[bool]) {
    for block in &func.blocks {
        let insts = &block.insts;
        let at: HashMap<ValueId, usize> = insts.iter().enumerate().map(|(k, i)| (i.dest(), k)).collect();
        let inline = |v: ValueId, tree: &[bool]| tree[v.index()] || fused[v.index()];
        // The words each top-level item pushes, in order: an inline
        // operand's own (`Some`), or a load of a slot (`None`).
        fn pushes(v: ValueId, at: &HashMap<ValueId, usize>, insts: &[MirInst], inline: &dyn Fn(ValueId) -> bool, seq: &mut Vec<Option<usize>>) {
            match at.get(&v) {
                Some(&k) if inline(v) => {
                    // A payload's scrutinee is not loaded for it.
                    if !matches!(insts[k], MirInst::MatchPayload { .. }) {
                        for op in insts[k].operands() {
                            pushes(op, at, insts, inline, seq);
                        }
                    }
                    seq.push(Some(k));
                }
                _ => seq.push(None),
            }
        }
        for (k, inst) in insts.iter().enumerate() {
            let MirInst::MatchPayload { dest, .. } = inst else { continue };
            if !tree[dest.index()] || is_jim_term_payload(func, *dest) {
                continue;
            }
            let is_inline = |v: ValueId| inline(v, tree);
            let mut first = None;
            let tops = insts
                .iter()
                .filter(|i| !i.is_phi() && !matches!(i, MirInst::MatchPayload { .. }) && !is_inline(i.dest()))
                .map(|i| i.operands())
                .chain(block.term.as_ref().map(term_values));
            for operands in tops {
                let mut seq = Vec::new();
                for op in operands {
                    pushes(op, &at, insts, &is_inline, &mut seq);
                }
                if seq.contains(&Some(k)) {
                    first = seq.first().copied().flatten();
                    break;
                }
            }
            if first != Some(k) {
                tree[dest.index()] = false;
            }
        }
    }
}

fn fused_cmp_dests(func: &MirFunc) -> Vec<bool> {
    let mut fused = vec![false; func.types.len()];
    for block in &func.blocks {
        if let Some(d) = term_cmp_dest(func, block) {
            fused[d.index()] = true;
        }
    }
    fused
}

fn term_values(term: &Terminator) -> Vec<ValueId> {
    match term {
        Terminator::Br { cond, .. } => vec![*cond],
        Terminator::JumpIfMatch {
            scrutinee,
            payloads,
            ..
        } => {
            let mut v = vec![*scrutinee];
            v.extend(payloads.iter().copied());
            v
        }
        Terminator::Return { lo, hi } => lo.iter().chain(hi.iter()).copied().collect(),
        _ => Vec::new(),
    }
}

/// `JumpIfMatch` leaves the scrutinee (miss) or payloads (taken) on the
/// stack. Store live payload slots; miss keeps TOS for the next match.
fn is_jim_term_payload(func: &MirFunc, dest: ValueId) -> bool {
    func.blocks.iter().any(|b| {
        matches!(
            &b.term,
            Some(Terminator::JumpIfMatch { payloads, .. }) if payloads.contains(&dest)
        )
    })
}

fn consume_match_tos(
    out: &mut Vec<IlOp>,
    func: &MirFunc,
    block: BlockId,
    plan: &EmitPlan,
    regs: &[u8],
    loc: DebugLoc,
) {
    for pred in &func.preds()[block.index()] {
        let Some(Terminator::JumpIfMatch {
            taken,
            payloads,
            ..
        }) = &func.block(*pred).term
        else {
            continue;
        };
        if *taken != block {
            continue;
        }
        if payloads.is_empty() {
            continue;
        }
        if payloads.len() > 1 || payloads.iter().any(|d| plan.need_slot[d.index()]) {
            for dest in payloads.iter().rev() {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
    }
}

fn last_arm_payloads(func: &MirFunc, block: BlockId, scrutinee: ValueId) -> Vec<ValueId> {
    let mut group = Vec::new();
    for inst in &func.block(block).insts {
        let MirInst::MatchPayload {
            dest,
            scrutinee: src,
            index,
        } = inst
        else {
            continue;
        };
        if *src != scrutinee || is_jim_term_payload(func, *dest) {
            continue;
        }
        let i = *index as usize;
        if group.len() <= i {
            group.resize(i + 1, *dest);
        }
        group[i] = *dest;
    }
    group
}

struct EmitLastArmUnpackArgs<'args> {
    out: &'args mut Vec<IlOp>,
    func: &'args MirFunc,
    block: BlockId,
    scrutinee: ValueId,
    plan: &'args EmitPlan,
    regs: &'args [u8],
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

fn emit_last_arm_unpack(args: EmitLastArmUnpackArgs<'_>) -> Result<(), LowerError> {
    let EmitLastArmUnpackArgs {
        out,
        func,
        block,
        scrutinee,
        plan,
        regs,
        pool,
        loc,
    } = args;
    let group = last_arm_payloads(func, block, scrutinee);
    let arity = group.len() as u32;
    if arity == 0 {
        return Ok(());
    }
    // `Unpack` pops the scrutinee. A `JumpIfMatch` miss leaves it on the
    // stack; otherwise (a single-variant match, or a nested pattern on a
    // payload) it lives in a register or is an inline tree (#606).
    if !super::emit::scrutinee_left_by_miss(func, block, scrutinee) {
        emit_stack(out, scrutinee, func, plan, regs, pool, loc)?;
    }
    out.push(IlOp::from_plain_byte(
        Byte::new(Instruction::Unpack).with_operand_u32(arity),
        loc,
    ));
    if group.len() > 1 || group.iter().any(|d| plan.need_slot[d.index()]) {
        for dest in group.iter().rev() {
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
    }
    Ok(())
}

fn assign_needed(func: &MirFunc, plan: &EmitPlan) -> Result<(Vec<u8>, u8), LowerError> {
    super::emit::assign_regs_prefer_payloads(func, &plan.need_slot)
}

/// `v` is `x & 1`: its word is 0 or 1.
fn is_low_bit(func: &MirFunc, plan: &EmitPlan, v: ValueId) -> bool {
    let Some((bid, idx)) = plan.def[v.index()] else {
        return false;
    };
    match &func.block(bid).insts[idx] {
        MirInst::Bin {
            op: MirBinOp::BitAnd,
            lhs,
            rhs,
            ..
        } => [*lhs, *rhs].into_iter().any(|m| {
            plan.def[m.index()].is_some_and(|(b, i)| {
                matches!(
                    &func.block(b).insts[i],
                    MirInst::Const { c: MirConst::I64(1) | MirConst::I32(1), .. }
                )
            })
        }),
        _ => false,
    }
}

fn tree_i16(func: &MirFunc, plan: &EmitPlan, v: ValueId) -> Option<i16> {
    if !plan.tree[v.index()] {
        return None;
    }
    let (bid, idx) = plan.def[v.index()]?;
    let MirInst::Const { c, .. } = &func.block(bid).insts[idx] else {
        return None;
    };
    let n = match *c {
        MirConst::I64(x) => x,
        MirConst::I32(x) => i64::from(x),
        MirConst::Bool(x) => i64::from(x),
        _ => return None,
    };
    i16::try_from(n).ok()
}

struct EmitBinArgs<'args> {
    out: &'args mut Vec<IlOp>,
    op: Instruction,
    lhs: ValueId,
    rhs: ValueId,
    func: &'args MirFunc,
    plan: &'args EmitPlan,
    regs: &'args [u8],
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

/// Prefer `BinSlotImm` / `BinSlotSlot` so pre-fuse cost matches opted fuse-IL.
fn emit_bin(args: EmitBinArgs<'_>) -> Result<(), LowerError> {
    let EmitBinArgs {
        out,
        op,
        lhs,
        rhs,
        func,
        plan,
        regs,
        pool,
        loc,
    } = args;

    if plan.need_slot[lhs.index()]
        && let Some(imm) = tree_i16(func, plan, rhs)
    {
        out.push(IlOp::BinSlotImm {
            op: op as u8,
            slot: regs[lhs.index()],
            imm,
            loc,
        });
        return Ok(());
    }
    if plan.need_slot[lhs.index()] && plan.need_slot[rhs.index()] {
        out.push(IlOp::BinSlotSlot {
            op: op as u8,
            a: regs[lhs.index()],
            b: regs[rhs.index()],
            loc,
        });
        return Ok(());
    }
    emit_stack(out, lhs, func, plan, regs, pool, loc)?;
    emit_stack(out, rhs, func, plan, regs, pool, loc)?;
    out.push(IlOp::Bin { op, loc });
    Ok(())
}

fn emit_stored(
    out: &mut Vec<IlOp>,
    inst: &MirInst,
    func: &MirFunc,
    plan: &EmitPlan,
    regs: &[u8],
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    match inst {
        MirInst::Const { dest, c } => {
            push_const(out, *c, pool, loc)?;
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Bin {
            dest,
            op,
            ty,
            lhs,
            rhs,
        } => {
            emit_bin(EmitBinArgs {
                out,
                op: stack_bin(*op, *ty)?,
                lhs: *lhs,
                rhs: *rhs,
                func,
                plan,
                regs,
                pool,
                loc,
            })?;
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Cmp {
            dest,
            op,
            ty,
            lhs,
            rhs,
        } => {
            emit_stack(out, *lhs, func, plan, regs, pool, loc)?;
            emit_stack(out, *rhs, func, plan, regs, pool, loc)?;
            out.push(IlOp::Bin {
                op: stack_cmp(*op, *ty)?,
                loc,
            });
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Unary { dest, op, src } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            push_unary(out, *op, func.ty(*src), loc);
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Cast {
            dest,
            kind,
            src,
            ..
        } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            push_cast(out, *kind, loc)?;
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Phi { .. } | MirInst::MatchPayload { .. } => {}
        MirInst::FieldLoad { dest, object, .. } => {
            emit_stack(out, *object, func, plan, regs, pool, loc)?;
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::HeapFieldLoad {
            dest,
            object,
            name,
            index,
        } => {
            emit_heap_field_load(EmitHeapFieldLoadArgs {
                out,
                object: *object,
                name: *name,
                index: *index,
                func,
                plan,
                regs,
                pool,
                loc,
            })?;
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::HeapFieldStore {
            dest,
            object,
            value,
            name,
            index,
        } => {
            emit_heap_field_store(EmitHeapFieldStoreArgs {
                out,
                object: *object,
                value: *value,
                name: *name,
                index: *index,
                func,
                plan,
                regs,
                pool,
                loc,
            })?;
            if plan.need_slot[dest.index()] {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::FieldStore {
            dest,
            src,
            base,
            index,
            ..
        } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            out.push(IlOp::StorePop {
                slot: *base + *index,
                loc,
            });
            if plan.need_slot[dest.index()] {
                out.push(IlOp::Load {
                    slot: *base + *index,
                    loc,
                });
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::Index {
            dest,
            array,
            index,
            unchecked,
        } => {
            emit_stack(out, *array, func, plan, regs, pool, loc)?;
            emit_stack(out, *index, func, plan, regs, pool, loc)?;
            if *unchecked {
                out.push(IlOp::IndexUnchecked { loc });
            } else {
                out.push(IlOp::Index { loc });
            }
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::StoreIndex {
            dest,
            array,
            index,
            value,
            unchecked,
        } => {
            emit_stack(out, *array, func, plan, regs, pool, loc)?;
            emit_stack(out, *index, func, plan, regs, pool, loc)?;
            emit_stack(out, *value, func, plan, regs, pool, loc)?;
            let inst = if *unchecked {
                Instruction::StoreIndexUnchecked
            } else {
                Instruction::StoreIndex
            };
            out.push(IlOp::from_plain_byte(Byte::new(inst), loc));
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::ArrayLen { dest, array } => {
            emit_stack(out, *array, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::ArrayLen), loc));
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::ResumeCoro {
            dest,
            handle,
            send,
        } => {
            emit_lir_resume(EmitLirResumeArgs {
                out,
                dest: *dest,
                handle: *handle,
                send: *send,
                func,
                plan,
                regs,
                pool,
                loc,
                always_store: true,
            })?;
        }
        MirInst::Yield {
            dest,
            value,
            from,
            receive,
        } => {
            emit_stack(out, *value, func, plan, regs, pool, loc)?;
            let op = if *from { Instruction::YieldFromCoro } else { Instruction::YieldCoro };
            out.push(IlOp::from_plain_byte(Byte::new(op), loc));
            // The VM pushes the sent word only for a store right after.
            if *receive {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::DoneCoro { dest, handle } => {
            emit_stack(out, *handle, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::DoneCoro), loc));
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::LoadStatic { dest, slot } => {
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::LoadStatic).with_operand_u32(*slot), loc));
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::StoreStatic { slot, value, .. } => {
            emit_stack(out, *value, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::StoreStatic).with_operand_u32(*slot), loc));
        }
        MirInst::HomeLoad { dest, home } => {
            out.push(IlOp::Load { slot: plan.home_slot(*home), loc });
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::HomeStore { home, value, .. } => {
            emit_stack(out, *value, func, plan, regs, pool, loc)?;
            out.push(IlOp::StorePop { slot: plan.home_slot(*home), loc });
        }
        // Emitted where it stands (`cleared_words`).
        MirInst::ClearWord { .. } | MirInst::KeepAlive { .. } => {}
        MirInst::ArrayPush {
            dest,
            array,
            value,
        } => {
            emit_stack(out, *array, func, plan, regs, pool, loc)?;
            emit_stack(out, *value, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::ArrayPush), loc));
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::HostInvoke {
            dest,
            native_id,
            layout,
            args,
        } => {
            // Same stack form as fuse-IL: native id, args, HostInvoke.
            out.push(IlOp::Const {
                imm: i32::from(*native_id),
                loc,
            });
            for a in args {
                emit_stack(out, *a, func, plan, regs, pool, loc)?;
            }
            out.push(IlOp::HostInvoke {
                arity: args.len() as u32,
                layout: *layout,
                loc,
            });
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Ffi { dest, op, operand, args } => {
            // As HIR emission: the operands in order, then the opcode.
            for a in args {
                emit_stack(out, *a, func, plan, regs, pool, loc)?;
            }
            out.push(IlOp::from_plain_byte(Byte::new(op.instruction()).with_operand_u32(*operand), loc));
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::CallIndirect { dest, callee, args, dicts } => {
            // The arguments, then the function value on top.
            for a in args.iter().chain([callee]) {
                emit_stack(out, *a, func, plan, regs, pool, loc)?;
            }
            let operand = super::emit::call_indirect_operand(args.len(), *dicts);
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::CallIndirect).with_operand_u32(operand), loc));
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Call {
            dest,
            dest_hi,
            target,
            args,
        } => {
            emit_lir_call(EmitLirCallArgs {
                out,
                dest: *dest,
                dest_hi: *dest_hi,
                target: *target,
                args,
                func,
                plan,
                regs,
                pool,
                loc,
            })?;
        }
        MirInst::Alloc { dest, kind, elems, kinds } => {
            emit_alloc_stack(EmitAllocStackArgs {
                out,
                kind: *kind,
                kinds: *kinds,
                elems,
                func,
                plan,
                regs,
                pool,
                loc,
            })?;
            if super::emit::dead_drop_alloc(func, *kind, *dest) {
                out.push(IlOp::Pop { loc });
            } else {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::GcBarrier { dest, .. } => {
            if let Some(obj) = paired_alloc_dest(func, *dest) {
                emit_stack(out, obj, func, plan, regs, pool, loc)?;
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::Deopt { .. } => {}
        MirInst::CodePtr { dest, target } => {
            out.push(super::emit::code_ptr_op(*target, loc));
            if plan.need_slot[dest.index()] {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::String { dest, idx } => {
            out.push(IlOp::String { idx: *idx, loc });
            if plan.need_slot[dest.index()] {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::Print { src, .. } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            out.push(IlOp::Print { loc });
        }
        MirInst::Panic { msg, .. } => {
            emit_stack(out, *msg, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::Panic), loc));
        }
        MirInst::Format { dest, fmt, args } => {
            emit_stack(out, *fmt, func, plan, regs, pool, loc)?;
            for a in args {
                emit_stack(out, *a, func, plan, regs, pool, loc)?;
            }
            out.push(IlOp::from_plain_byte(
                Byte::new(Instruction::FORMAT).with_operand_u32(args.len() as u32),
                loc,
            ));
            if plan.need_slot[dest.index()] {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::Stringify { dest, src } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::STRINGIFY), loc));
            if plan.need_slot[dest.index()] {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::TagArrayKind { dest, src, kind } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::TagArrayKind).with_operand_u32(u32::from(*kind)), loc));
            if plan.need_slot[dest.index()] {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::Unbox { dest, src, tag } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            out.push(IlOp::UnboxValue { tag: *tag, loc });
            if plan.need_slot[dest.index()] {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::StrEq { dest, lhs, rhs, ne } => {
            emit_stack(out, *lhs, func, plan, regs, pool, loc)?;
            emit_stack(out, *rhs, func, plan, regs, pool, loc)?;
            out.push(IlOp::Bin {
                op: str_eq_op(*ne),
                loc,
            });
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
    }
    Ok(())
}

struct EmitLirCallArgs<'args> {
    out: &'args mut Vec<IlOp>,
    dest: ValueId,
    dest_hi: Option<ValueId>,
    target: crate::il::Label,
    args: &'args [ValueId],
    func: &'args MirFunc,
    plan: &'args EmitPlan,
    regs: &'args [u8],
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

fn emit_lir_call(args: EmitLirCallArgs<'_>) -> Result<(), LowerError> {
    let EmitLirCallArgs {
        out,
        dest,
        dest_hi,
        target,
        args,
        func,
        plan,
        regs,
        pool,
        loc,
    } = args;

    for a in args {
        emit_stack(out, *a, func, plan, regs, pool, loc)?;
    }
    out.push(IlOp::Entry {
        kind: crate::il::EntryKind::Call,
        arity: args.len() as u32,
        target,
        loc,
        ret_words: super::abi::ret_words_from_hi(dest_hi),
    });
    if let Some(hi) = dest_hi {
        out.push(IlOp::StorePop {
            slot: u32::from(regs[hi.index()]),
            loc,
        });
    }
    if plan.need_slot[dest.index()] {
        out.push(IlOp::StorePop {
            slot: u32::from(regs[dest.index()]),
            loc,
        });
    }
    Ok(())
}

struct EmitAllocStackArgs<'args> {
    out: &'args mut Vec<IlOp>,
    kind: MirAllocKind,
    kinds: u8,
    elems: &'args [ValueId],
    func: &'args MirFunc,
    plan: &'args EmitPlan,
    regs: &'args [u8],
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

fn emit_alloc_stack(args: EmitAllocStackArgs<'_>) -> Result<(), LowerError> {
    let EmitAllocStackArgs {
        out,
        kind,
        kinds,
        elems,
        func,
        plan,
        regs,
        pool,
        loc,
    } = args;

    for e in elems {
        emit_stack(out, *e, func, plan, regs, pool, loc)?;
    }
    out.extend(il_for_alloc(kind, elems.len() as u32, kinds, loc)?);
    Ok(())
}

struct EmitLirResumeArgs<'args> {
    out: &'args mut Vec<IlOp>,
    dest: ValueId,
    handle: ValueId,
    send: Option<ValueId>,
    func: &'args MirFunc,
    plan: &'args EmitPlan,
    regs: &'args [u8],
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
    always_store: bool,
}

fn emit_lir_resume(args: EmitLirResumeArgs<'_>) -> Result<(), LowerError> {
    let EmitLirResumeArgs {
        out,
        dest,
        handle,
        send,
        func,
        plan,
        regs,
        pool,
        loc,
        always_store,
    } = args;

    if let Some(s) = send {
        emit_stack(out, s, func, plan, regs, pool, loc)?;
    }
    emit_stack(out, handle, func, plan, regs, pool, loc)?;
    out.push(IlOp::from_plain_byte(
        Byte::new(Instruction::ResumeCoro).with_operand_u32(u32::from(send.is_some())),
        loc,
    ));
    if always_store || plan.need_slot[dest.index()] {
        if !always_store {
            out.push(IlOp::Dup { loc });
        }
        out.push(IlOp::StorePop {
            slot: u32::from(regs[dest.index()]),
            loc,
        });
    }
    Ok(())
}

struct EmitHiAfterLoArgs<'args> {
    out: &'args mut Vec<IlOp>,
    lo: ValueId,
    hi: ValueId,
    func: &'args MirFunc,
    plan: &'args EmitPlan,
    regs: &'args [u8],
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

/// `return k, k + 1` after `k` is already TOS: `DUP; CONST 1; ADD`.
fn emit_hi_after_lo(args: EmitHiAfterLoArgs<'_>) -> Result<(), LowerError> {
    let EmitHiAfterLoArgs {
        out,
        lo,
        hi,
        func,
        plan,
        regs,
        pool,
        loc,
    } = args;

    if plan.tree[hi.index()]
        && let Some((bid, idx)) = plan.def[hi.index()]
        && let MirInst::Bin { op, ty, lhs, rhs, .. } = &func.block(bid).insts[idx]
        && *lhs == lo
    {
        out.push(IlOp::Dup { loc });
        emit_stack(out, *rhs, func, plan, regs, pool, loc)?;
        out.push(IlOp::Bin {
            op: stack_bin(*op, *ty)?,
            loc,
        });
        return Ok(());
    }
    emit_stack(out, hi, func, plan, regs, pool, loc)
}

struct EmitHeapFieldLoadArgs<'args> {
    out: &'args mut Vec<IlOp>,
    object: ValueId,
    name: Option<ValueId>,
    index: u32,
    func: &'args MirFunc,
    plan: &'args EmitPlan,
    regs: &'args [u8],
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

fn emit_heap_field_load(args: EmitHeapFieldLoadArgs<'_>) -> Result<(), LowerError> {
    let EmitHeapFieldLoadArgs {
        out,
        object,
        name,
        index,
        func,
        plan,
        regs,
        pool,
        loc,
    } = args;

    emit_stack(out, object, func, plan, regs, pool, loc)?;
    if let Some(n) = name {
        emit_stack(out, n, func, plan, regs, pool, loc)?;
        out.push(IlOp::GetField { loc });
    } else {
        out.push(IlOp::LoadField { index, loc });
    }
    Ok(())
}

struct EmitHeapFieldStoreArgs<'args> {
    out: &'args mut Vec<IlOp>,
    object: ValueId,
    value: ValueId,
    name: Option<ValueId>,
    index: Option<u32>,
    func: &'args MirFunc,
    plan: &'args EmitPlan,
    regs: &'args [u8],
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

fn emit_heap_field_store(args: EmitHeapFieldStoreArgs<'_>) -> Result<(), LowerError> {
    let EmitHeapFieldStoreArgs {
        out,
        object,
        value,
        name,
        index,
        func,
        plan,
        regs,
        pool,
        loc,
    } = args;

    emit_stack(out, value, func, plan, regs, pool, loc)?;
    emit_stack(out, object, func, plan, regs, pool, loc)?;
    if let Some(n) = name {
        emit_stack(out, n, func, plan, regs, pool, loc)?;
        out.push(IlOp::SetField { index: None, loc });
    } else {
        out.push(IlOp::SetField { index, loc });
    }
    Ok(())
}

fn emit_stack(
    out: &mut Vec<IlOp>,
    v: ValueId,
    func: &MirFunc,
    plan: &EmitPlan,
    regs: &[u8],
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    if !plan.tree[v.index()] {
        out.push(IlOp::Load {
            slot: u32::from(regs[v.index()]),
            loc,
        });
        return Ok(());
    }
    let Some((bid, idx)) = plan.def[v.index()] else {
        return Err(LowerError::Refused("tree value has no def".into()));
    };
    let inst = &func.block(bid).insts[idx];
    // A forwarded op that can panic keeps its own line, not its
    // consumer's (#885); so does an inline call, the frame a trace names.
    let own = func.op_loc(v);
    let keeps_own = inst.may_trap(func) || matches!(inst, MirInst::Call { .. });
    let loc = if own.is_known() && keeps_own { own } else { loc };
    match inst {
        MirInst::Const { c, .. } => push_const(out, *c, pool, loc),
        MirInst::Bin {
            op, ty, lhs, rhs, ..
        } => emit_bin(EmitBinArgs {
            out,
            op: stack_bin(*op, *ty)?,
            lhs: *lhs,
            rhs: *rhs,
            func,
            plan,
            regs,
            pool,
            loc,
        }),
        MirInst::Cmp {
            op, ty, lhs, rhs, ..
        } => emit_bin(EmitBinArgs {
            out,
            op: stack_cmp(*op, *ty)?,
            lhs: *lhs,
            rhs: *rhs,
            func,
            plan,
            regs,
            pool,
            loc,
        }),
        MirInst::Unary { op, src, .. } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            push_unary(out, *op, func.ty(*src), loc);
            Ok(())
        }
        MirInst::Cast { kind, src, .. } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            push_cast(out, *kind, loc)
        }
        MirInst::Phi { dest, .. } => {
            out.push(IlOp::Load {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
            Ok(())
        }
        MirInst::Index {
            dest,
            array,
            index,
            unchecked,
        } => {
            emit_stack(out, *array, func, plan, regs, pool, loc)?;
            emit_stack(out, *index, func, plan, regs, pool, loc)?;
            if *unchecked {
                out.push(IlOp::IndexUnchecked { loc });
            } else {
                out.push(IlOp::Index { loc });
            }
            if plan.need_slot[dest.index()] {
                out.push(IlOp::Dup { loc });
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
            Ok(())
        }
        MirInst::StoreIndex {
            dest,
            array,
            index,
            value,
            unchecked,
        } => {
            emit_stack(out, *array, func, plan, regs, pool, loc)?;
            emit_stack(out, *index, func, plan, regs, pool, loc)?;
            emit_stack(out, *value, func, plan, regs, pool, loc)?;
            let inst = if *unchecked {
                Instruction::StoreIndexUnchecked
            } else {
                Instruction::StoreIndex
            };
            out.push(IlOp::from_plain_byte(Byte::new(inst), loc));
            if plan.need_slot[dest.index()] {
                out.push(IlOp::Dup { loc });
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
            Ok(())
        }
        MirInst::ArrayLen { dest, array } => {
            emit_stack(out, *array, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::ArrayLen), loc));
            if plan.need_slot[dest.index()] {
                out.push(IlOp::Dup { loc });
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
            Ok(())
        }
        MirInst::ResumeCoro {
            dest,
            handle,
            send,
        } => emit_lir_resume(EmitLirResumeArgs {
            out,
            dest: *dest,
            handle: *handle,
            send: *send,
            func,
            plan,
            regs,
            pool,
            loc,
            always_store: false,
        }),
        MirInst::Yield {
            dest,
            value,
            from,
            receive,
        } => {
            emit_stack(out, *value, func, plan, regs, pool, loc)?;
            let op = if *from { Instruction::YieldFromCoro } else { Instruction::YieldCoro };
            out.push(IlOp::from_plain_byte(Byte::new(op), loc));
            // The VM pushes the sent word only for a store right after.
            if *receive {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
            Ok(())
        }
        MirInst::DoneCoro { dest, handle } => {
            emit_stack(out, *handle, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::DoneCoro), loc));
            if plan.need_slot[dest.index()] {
                out.push(IlOp::Dup { loc });
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
            Ok(())
        }
        MirInst::LoadStatic { dest, slot } => {
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::LoadStatic).with_operand_u32(*slot), loc));
            if plan.need_slot[dest.index()] {
                out.push(IlOp::Dup { loc });
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
            Ok(())
        }
        MirInst::StoreStatic { slot, value, .. } => {
            emit_stack(out, *value, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::StoreStatic).with_operand_u32(*slot), loc));
            Ok(())
        }
        MirInst::HomeLoad { dest, home } => {
            out.push(IlOp::Load { slot: plan.home_slot(*home), loc });
            if plan.need_slot[dest.index()] {
                out.push(IlOp::Dup { loc });
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
            Ok(())
        }
        MirInst::HomeStore { home, value, .. } => {
            emit_stack(out, *value, func, plan, regs, pool, loc)?;
            out.push(IlOp::StorePop { slot: plan.home_slot(*home), loc });
            Ok(())
        }
        MirInst::ClearWord { .. } | MirInst::KeepAlive { .. } => Ok(()),
        MirInst::ArrayPush {
            dest,
            array,
            value,
        } => {
            emit_stack(out, *array, func, plan, regs, pool, loc)?;
            emit_stack(out, *value, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::ArrayPush), loc));
            if plan.need_slot[dest.index()] {
                out.push(IlOp::Dup { loc });
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
            Ok(())
        }
        // A one-word call with a single use in its block: emitted where
        // that use is (`keep_memory_order` keeps it in program order), its
        // result left on the stack. A pair call keeps its slots.
        MirInst::Call {
            dest,
            dest_hi: None,
            target,
            args,
        } => emit_lir_call(EmitLirCallArgs {
            out,
            dest: *dest,
            dest_hi: None,
            target: *target,
            args,
            func,
            plan,
            regs,
            pool,
            loc,
        }),
        // Host calls and indirect calls always keep a slot (`EmitPlan`).
        MirInst::HostInvoke { .. } | MirInst::Ffi { .. } | MirInst::Call { .. } | MirInst::CallIndirect { .. } => Err(LowerError::Refused(
            "MIR→LIR leafs do not emit HostInvoke/CALL (dense W4/M2)".into(),
        )),
        MirInst::MatchPayload { dest, .. } => {
            if is_jim_term_payload(func, *dest)
                && plan.need_slot[dest.index()] {
                    out.push(IlOp::Load {
                        slot: u32::from(regs[dest.index()]),
                        loc,
                    });
                }
            Ok(())
        }
        MirInst::FieldLoad { object, .. } => emit_stack(out, *object, func, plan, regs, pool, loc),
        MirInst::HeapFieldLoad {
            object,
            name,
            index,
            ..
        } => emit_heap_field_load(EmitHeapFieldLoadArgs {
            out,
            object: *object,
            name: *name,
            index: *index,
            func,
            plan,
            regs,
            pool,
            loc,
        }),
        MirInst::HeapFieldStore {
            object,
            value,
            name,
            index,
            ..
        } => emit_heap_field_store(EmitHeapFieldStoreArgs {
            out,
            object: *object,
            value: *value,
            name: *name,
            index: *index,
            func,
            plan,
            regs,
            pool,
            loc,
        }),
        MirInst::FieldStore { src, .. } => emit_stack(out, *src, func, plan, regs, pool, loc),
        MirInst::Alloc { kind, elems, kinds, .. } => {
            emit_alloc_stack(EmitAllocStackArgs {
                out,
                kind: *kind,
                kinds: *kinds,
                elems,
                func,
                plan,
                regs,
                pool,
                loc,
            })
        }
        MirInst::GcBarrier { dest, .. } => {
            if let Some(obj) = paired_alloc_dest(func, *dest) {
                emit_stack(out, obj, func, plan, regs, pool, loc)
            } else if plan.need_slot[dest.index()] {
                out.push(IlOp::Load {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
                Ok(())
            } else {
                Err(LowerError::Refused("GcBarrier has no alloc".into()))
            }
        }
        MirInst::Deopt { .. } => Ok(()),
        MirInst::String { idx, .. } => {
            out.push(IlOp::String { idx: *idx, loc });
            Ok(())
        }
        MirInst::CodePtr { target, .. } => {
            out.push(super::emit::code_ptr_op(*target, loc));
            Ok(())
        }
        MirInst::Print { src, .. } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            out.push(IlOp::Print { loc });
            Ok(())
        }
        MirInst::Panic { msg, .. } => {
            emit_stack(out, *msg, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::Panic), loc));
            Ok(())
        }
        MirInst::Format { fmt, args, .. } => {
            emit_stack(out, *fmt, func, plan, regs, pool, loc)?;
            for a in args {
                emit_stack(out, *a, func, plan, regs, pool, loc)?;
            }
            out.push(IlOp::from_plain_byte(
                Byte::new(Instruction::FORMAT).with_operand_u32(args.len() as u32),
                loc,
            ));
            Ok(())
        }
        MirInst::Stringify { src, .. } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::STRINGIFY), loc));
            Ok(())
        }
        MirInst::Unbox { src, tag, .. } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            out.push(IlOp::UnboxValue { tag: *tag, loc });
            Ok(())
        }
        MirInst::TagArrayKind { src, kind, .. } => {
            emit_stack(out, *src, func, plan, regs, pool, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::TagArrayKind).with_operand_u32(u32::from(*kind)), loc));
            Ok(())
        }
        MirInst::StrEq { lhs, rhs, ne, .. } => {
            emit_stack(out, *lhs, func, plan, regs, pool, loc)?;
            emit_stack(out, *rhs, func, plan, regs, pool, loc)?;
            out.push(IlOp::Bin {
                op: str_eq_op(*ne),
                loc,
            });
            Ok(())
        }
    }
}

/// VM `EQ` / `NEQ`: strings compare by content.
fn str_eq_op(ne: bool) -> Instruction {
    if ne {
        Instruction::NEQ
    } else {
        Instruction::EQ
    }
}

fn push_unary(out: &mut Vec<IlOp>, op: MirUnaryOp, ty: MirTy, loc: DebugLoc) {
    match (op, ty.is_float()) {
        (MirUnaryOp::Not, _) => out.push(IlOp::LogNot { loc }),
        (MirUnaryOp::Neg, true) => out.push(IlOp::from_plain_byte(Byte::new(Instruction::NEGF), loc)),
        (MirUnaryOp::Neg, false) => out.push(IlOp::from_plain_byte(Byte::new(Instruction::NEG), loc)),
    }
}

fn push_cast(out: &mut Vec<IlOp>, kind: MirCastKind, loc: DebugLoc) -> Result<(), LowerError> {
    match kind {
        MirCastKind::IntToFloat => {
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::CastIntToFloat), loc));
            Ok(())
        }
        MirCastKind::FloatToInt => {
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::CastFloatToInt), loc));
            Ok(())
        }
        MirCastKind::Sext => Err(LowerError::Refused("lir sext".into())),
        // The same word, retyped.
        MirCastKind::Word => Ok(()),
    }
}

struct EmitTermArgs<'args> {
    out: &'args mut Vec<IlOp>,
    block: &'args super::func::MirBlock,
    func: &'args MirFunc,
    plan: &'args EmitPlan,
    regs: &'args [u8],
    scratch: u8,
    block_lab: &'args [Label],
    next_label: &'args mut u32,
    reserved: &'args std::collections::HashSet<u32>,
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

/// The call `block` ends with when it is a tail call: its result is what
/// the block returns, and its callee may take the frame
/// ([`MirFunc::tail_targets`]).
fn tail_call<'f>(func: &MirFunc, block: &'f super::func::MirBlock) -> Option<&'f MirInst> {
    let inst = block.insts.last()?;
    match inst {
        MirInst::Call { .. } if super::call_convoy::is_tail_call_inst(func, block, inst) => Some(inst),
        _ => None,
    }
}

fn emit_term(args: EmitTermArgs<'_>) -> Result<(), LowerError> {
    let EmitTermArgs {
        out,
        block,
        func,
        plan,
        regs,
        scratch,
        block_lab,
        next_label,
        reserved,
        pool,
        loc,
    } = args;

    let Some(term) = &block.term else {
        return Err(LowerError::Refused("missing terminator".into()));
    };
    match term {
        Terminator::Jump { dest } => {
            emit_phi_moves(out, func, block.id, *dest, regs, scratch, loc);
            if !is_fallthrough(func, block.id, *dest) {
                out.push(IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: block_lab[dest.index()],
                    loc,
                    hint: Default::default(),
                });
            }
        }
        Terminator::Br {
            cond,
            taken,
            not_taken,
        } => {
            let t_moves = phi_moves(func, block.id, *taken, regs, scratch);
            let f_moves = phi_moves(func, block.id, *not_taken, regs, scratch);
            emit_br_cond(EmitBrCondArgs {
                out,
                block,
                func,
                plan,
                regs,
                cond: *cond,
                pool,
                loc,
            })?;
            if t_moves.is_empty() && f_moves.is_empty() {
                emit_cond_jumps(out, func, block.id, *taken, *not_taken, block_lab, loc);
            } else if f_moves.is_empty() {
                // Only the taken edge moves: branch straight to `not_taken`.
                out.push(IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    target: block_lab[not_taken.index()],
                    loc,
                    hint: Default::default(),
                });
                for (d, s) in t_moves {
                    push_move(out, d, s, loc);
                }
                if !is_fallthrough(func, block.id, *taken) {
                    out.push(IlOp::Jump {
                        kind: IlJumpKind::Unconditional,
                        target: block_lab[taken.index()],
                        loc,
                        hint: Default::default(),
                    });
                }
            } else {
                // The false edge's moves sit between this block and the next
                // emitted one, so the taken path always jumps over them, even
                // when `taken` is the layout successor (#548: the true path
                // fell into the false edge's moves and reached `not_taken`).
                let f_lab = take_label(next_label, reserved);
                out.push(IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    target: f_lab,
                    loc,
                    hint: Default::default(),
                });
                for (d, s) in t_moves {
                    push_move(out, d, s, loc);
                }
                out.push(IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: block_lab[taken.index()],
                    loc,
                    hint: Default::default(),
                });
                out.push(IlOp::Label(f_lab));
                for (d, s) in f_moves {
                    push_move(out, d, s, loc);
                }
                if !is_fallthrough(func, block.id, *not_taken) {
                    out.push(IlOp::Jump {
                        kind: IlJumpKind::Unconditional,
                        target: block_lab[not_taken.index()],
                        loc,
                        hint: Default::default(),
                    });
                }
            }
        }
        Terminator::Return { .. } if let Some(MirInst::Call { args: call_args, target, dest_hi, .. }) = tail_call(func, block) => {
            // The callee takes this frame: its arguments, then `TailCall`.
            for a in call_args {
                emit_stack(out, *a, func, plan, regs, pool, loc)?;
            }
            out.push(IlOp::Entry {
                kind: crate::il::EntryKind::TailCall,
                arity: call_args.len() as u32,
                target: *target,
                loc,
                ret_words: super::abi::ret_words_from_hi(*dest_hi),
            });
        }
        Terminator::Return { lo, hi } => {
            let ret_words = super::abi::ret_words_from_hi(*hi);
            if super::abi::is_multi_word_ret(ret_words) && func.ret_layout != MirLayout::TwoSlot {
                return Err(LowerError::Refused("pair return without twoslot".into()));
            }
            if let Some(v) = lo {
                emit_stack(out, *v, func, plan, regs, pool, loc)?;
            } else if ret_words == 1 {
                out.push(IlOp::Const { imm: 0, loc });
            } else {
                return Err(LowerError::Refused("empty pair return".into()));
            }
            if let Some(v) = hi {
                if let Some(lo_v) = *lo {
                    emit_hi_after_lo(EmitHiAfterLoArgs {
                        out,
                        lo: lo_v,
                        hi: *v,
                        func,
                        plan,
                        regs,
                        pool,
                        loc,
                    })?;
                } else {
                    emit_stack(out, *v, func, plan, regs, pool, loc)?;
                }
            }
            out.push(IlOp::Return { loc, ret_words });
        }
        Terminator::Unreachable => {
            out.push(IlOp::Halt { loc });
        }
        Terminator::JumpIfMatch {
            scrutinee,
            tag,
            taken,
            not_taken,
            ..
        } => {
            let _ = tag;
            if !phi_moves(func, block.id, *taken, regs, scratch).is_empty()
                || !phi_moves(func, block.id, *not_taken, regs, scratch).is_empty()
            {
                return Err(LowerError::Refused(
                    "JumpIfMatch + phi moves (keep fuse-IL)".into(),
                ));
            }
            if super::emit::miss_chain_tos(func, block.id).is_none() {
                emit_stack(out, *scrutinee, func, plan, regs, pool, loc)?;
            }
            out.push(IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch {
                    tag: *tag,
                    arity: func
                        .block(block.id)
                        .term
                        .as_ref()
                        .and_then(|t| match t {
                            Terminator::JumpIfMatch { payloads, .. } => {
                                Some(payloads.len() as u32)
                            }
                            _ => None,
                        })
                        .unwrap_or(0),
                },
                target: block_lab[taken.index()],
                loc,
                hint: Default::default(),
            });
            if !is_fallthrough(func, block.id, *not_taken) {
                out.push(IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: block_lab[not_taken.index()],
                    loc,
                    hint: Default::default(),
                });
            }
        }
    }
    Ok(())
}

struct EmitBrCondArgs<'args> {
    out: &'args mut Vec<IlOp>,
    block: &'args super::func::MirBlock,
    func: &'args MirFunc,
    plan: &'args EmitPlan,
    regs: &'args [u8],
    cond: ValueId,
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

fn emit_br_cond(args: EmitBrCondArgs<'_>) -> Result<(), LowerError> {
    let EmitBrCondArgs {
        out,
        block,
        func,
        plan,
        regs,
        cond,
        pool,
        loc,
    } = args;

    if let Some(MirInst::Cmp {
        op, ty, lhs, rhs, ..
    }) = block.insts.iter().find(|inst| {
        matches!(inst, MirInst::Cmp { dest, .. } if *dest == cond)
    }) {
        // `x != 0` on a 0-or-1 int word (`i & 1`): branch on `x` itself,
        // keeping `BinSlotImmJmp*` fusion. The jump tests a bool word, so
        // any other `x` (2 is not true) keeps the compare.
        if *op == MirCmpOp::Ne
            && ty.is_int()
            && tree_i16(func, plan, *rhs) == Some(0)
            && is_low_bit(func, plan, *lhs)
        {
            return emit_stack(out, *lhs, func, plan, regs, pool, loc);
        }
        return emit_bin(EmitBinArgs {
            out,
            op: stack_cmp(*op, *ty)?,
            lhs: *lhs,
            rhs: *rhs,
            func,
            plan,
            regs,
            pool,
            loc,
        });
    }
    emit_stack(out, cond, func, plan, regs, pool, loc)
}

fn emit_phi_moves(
    out: &mut Vec<IlOp>,
    func: &MirFunc,
    pred: BlockId,
    succ: BlockId,
    regs: &[u8],
    scratch: u8,
    loc: DebugLoc,
) {
    for (d, s) in phi_moves(func, pred, succ, regs, scratch) {
        push_move(out, d, s, loc);
    }
}

fn phi_moves(
    func: &MirFunc,
    pred: BlockId,
    succ: BlockId,
    regs: &[u8],
    scratch: u8,
) -> Vec<(u8, u8)> {
    let mut moves = Vec::new();
    for inst in &func.block(succ).insts {
        let MirInst::Phi { dest, args, .. } = inst else {
            break;
        };
        if let Some((_, src)) = args.iter().find(|(b, _)| *b == pred) {
            let d = regs[dest.index()];
            let s = regs[src.index()];
            if d != s {
                moves.push((d, s));
            }
        }
    }
    resolve_parallel(moves, scratch)
}

fn resolve_parallel(mut moves: Vec<(u8, u8)>, scratch: u8) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    while !moves.is_empty() {
        if let Some(i) = moves.iter().position(|(d, _)| !moves.iter().any(|(_, s)| s == d)) {
            out.push(moves.remove(i));
            continue;
        }
        let (d0, s0) = moves.remove(0);
        out.push((scratch, d0));
        out.push((d0, s0));
        for (_, s) in moves.iter_mut() {
            if *s == d0 {
                *s = scratch;
            }
        }
    }
    out
}

fn push_move(out: &mut Vec<IlOp>, dest: u8, src: u8, loc: DebugLoc) {
    if dest == src {
        return;
    }
    out.push(IlOp::Load {
        slot: u32::from(src),
        loc,
    });
    out.push(IlOp::StorePop {
        slot: u32::from(dest),
        loc,
    });
}

fn push_const(
    out: &mut Vec<IlOp>,
    c: MirConst,
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    match c {
        MirConst::I64(v) => {
            // Inline CONST uses bit 31 as POOL_FLAG; negatives must go through the pool.
            if let Ok(imm) = i32::try_from(v)
                && imm >= 0 {
                    out.push(IlOp::Const { imm, loc });
                    return Ok(());
                }
            let idx = intern_pool(pool, v as u64)?;
            out.push(IlOp::ConstPool { idx, loc });
        }
        MirConst::I32(v) => {
            if v >= 0 {
                out.push(IlOp::Const { imm: v, loc });
            } else {
                let idx = intern_pool(pool, v as i64 as u64)?;
                out.push(IlOp::ConstPool { idx, loc });
            }
        }
        MirConst::Bool(v) => out.push(IlOp::Const {
            imm: i32::from(v),
            loc,
        }),
        MirConst::F64(bits) => {
            let idx = intern_pool(pool, bits)?;
            out.push(IlOp::ConstPool { idx, loc });
        }
        MirConst::F32(bits) => {
            let idx = intern_pool(pool, u64::from(bits))?;
            out.push(IlOp::ConstPool { idx, loc });
        }
    }
    Ok(())
}

fn intern_pool(pool: &mut Vec<u64>, bits: u64) -> Result<u32, LowerError> {
    if let Some(i) = pool.iter().position(|&x| x == bits) {
        return u32::try_from(i).map_err(|_| LowerError::Refused("pool idx".into()));
    }
    let i = pool.len();
    pool.push(bits);
    u32::try_from(i).map_err(|_| LowerError::Refused("const pool full".into()))
}

fn stack_bin(op: MirBinOp, ty: MirTy) -> Result<Instruction, LowerError> {
    Ok(match (op, ty.is_float()) {
        (MirBinOp::Add, false) => Instruction::ADD,
        (MirBinOp::Add, true) => Instruction::ADDF,
        (MirBinOp::Sub, false) => Instruction::SUB,
        (MirBinOp::Sub, true) => Instruction::SUBF,
        (MirBinOp::Mul, false) => Instruction::MUL,
        (MirBinOp::Mul, true) => Instruction::MULF,
        (MirBinOp::Div, false) => Instruction::DIV,
        (MirBinOp::Div, true) => Instruction::DIVF,
        (MirBinOp::Rem, false) => Instruction::MOD,
        (MirBinOp::Rem, true) => Instruction::MODF,
        (MirBinOp::BitAnd, _) => Instruction::BITAND,
        (MirBinOp::BitOr, _) => Instruction::BITOR,
        (MirBinOp::Xor, _) => Instruction::XOR,
        (MirBinOp::Shl, _) => Instruction::SHL,
        (MirBinOp::Shr, _) => Instruction::SHR,
        (MirBinOp::Pow, true) => Instruction::PowF,
        (MirBinOp::Pow, false) => Instruction::Pow,
    })
}

fn stack_cmp(op: MirCmpOp, ty: MirTy) -> Result<Instruction, LowerError> {
    Ok(match (op, ty.is_float()) {
        (MirCmpOp::Lt, false) => Instruction::LE,
        (MirCmpOp::Le, false) => Instruction::LEQ,
        (MirCmpOp::Gt, false) => Instruction::GT,
        (MirCmpOp::Ge, false) => Instruction::GEQ,
        (MirCmpOp::Eq, false) => Instruction::EQ,
        (MirCmpOp::Ne, false) => Instruction::NEQ,
        (MirCmpOp::Lt, true) => Instruction::LEF,
        (MirCmpOp::Le, true) => Instruction::LEQF,
        (MirCmpOp::Gt, true) => Instruction::GTF,
        (MirCmpOp::Ge, true) => Instruction::GEQF,
        (MirCmpOp::Eq, true) => Instruction::EQ,
        (MirCmpOp::Ne, true) => Instruction::NEQ,
    })
}
