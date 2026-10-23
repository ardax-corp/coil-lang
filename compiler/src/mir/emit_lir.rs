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
    max_label_hint, restore_block_op_locs, take_label,
};
use super::emit_common::{
    const_i16, is_jim_term_payload, last_arm_payloads, phi_moves, push_const, stack_bin_op,
    stack_cmp_op, term_uses,
};
use super::func::MirFunc;
use super::inst::{
    BlockId, MirAllocKind, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, MirUnaryOp,
    Terminator, ValueId,
};
use crate::layout::MirLayout;
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
    emit_lir_framed(func, entry_label, aliases, pool, across_alloc).map(|(out, _)| out)
}

/// The frame LIR emission gave a body: what its sidecars and its cleanup
/// pad read, without planning the body again (#1502).
pub(super) struct LirFrame {
    regs: Vec<u8>,
    need_slot: Vec<bool>,
    /// `(home 0's word, frame words)` ([`lir_home_frame`]).
    home_frame: (u8, u32),
}

impl LirFrame {
    /// [`lir_sidecars`] of the body this frame is of.
    pub(super) fn sidecars(&self, func: &MirFunc) -> LirSidecars {
        (
            super::deopt::debug_slot_remap(func, &self.regs, &self.need_slot),
            super::deopt::encode_draft(func, &self.regs, &self.need_slot),
        )
    }

    pub(super) fn home_frame(&self) -> (u8, u32) {
        self.home_frame
    }
}

/// [`emit_lir_with_aliases`], with the frame it gave the body.
pub(super) fn emit_lir_framed(
    func: &MirFunc,
    entry_label: Option<Label>,
    aliases: &[Label],
    pool: &mut Vec<u64>,
    across_alloc: bool,
) -> Result<(Vec<IlOp>, LirFrame), LowerError> {
    if func.has_mapped_gc_edge() && !across_alloc {
        return Err(LowerError::Refused(
            "MIR→LIR refuses Alloc/GcBarrier without GC maps (S2c)".into(),
        ));
    }
    let mut plan = EmitPlan::new(func);
    let (regs, scratch) = assign_needed(func, &mut plan)?;
    plan.home_base = home_base(func, scratch)?;
    let plan = plan;
    let regs = coalesce_latch_overwrite(func, regs, &plan.need_slot);
    let mut cleared = super::emit::cleared_words(func, &regs, &plan.need_slot);
    let mut entry_clears = super::emit::entry_clears(func, &regs, &plan.need_slot);
    let mut source_clears = super::emit::source_clears(func, &regs, &plan.need_slot, &super::emit::Gathers::default());
    super::emit::prune_clears(func, &regs, &plan.need_slot, &mut cleared, &mut entry_clears, &mut source_clears, &[]);
    let roots = plan.tree_roots(func);
    let arg_clears = super::emit::arg_clears(func, &regs, &plan.need_slot, &super::emit::Gathers::default(), |d| {
        roots.get(d.index()).copied().flatten()
    });
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
        for &r in arg_clears.entry(block.id) {
            super::emit::push_clear_word(&mut out, r);
        }
        let term_loc = func.term_loc(block.id);
        consume_match_tos(&mut out, func, block.id, &plan, &regs, term_loc);
        // A miss leaves the scrutinee on the stack: the next arm's test
        // peeks it there; a block taking no payload of it pops it.
        let preds = &plan.preds[block.id.index()];
        if super::emit::miss_chain_tos(func, preds, block.id).is_none()
            && super::emit::missed_scrutinee_unused(func, preds, block.id)
        {
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
                for &r in source_clears.get(dest).map(Vec::as_slice).unwrap_or(&[]) {
                    super::emit::push_clear_word(&mut out, r);
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
            if plan.cmp_dests[block.id.index()].is_some_and(|d| dest == d) {
                continue;
            }
            emit_stored(&mut out, inst, func, &plan, &regs, pool, loc)?;
            for &r in arg_clears.after(dest) {
                super::emit::push_clear_word(&mut out, r);
            }
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
            clears: &entry_clears,
        })?;
    }
    let home_frame = (plan.home_base, (u32::from(max_reg) + 1).max(plan.home_slot(0) + func.homes.len() as u32));
    let EmitPlan { need_slot, .. } = plan;
    Ok((out, LirFrame { regs, need_slot, home_frame }))
}

pub(super) type LirSidecars = (std::collections::HashMap<u32, u32>, super::deopt::DraftDeoptMap);

/// Debug slot remap and deopt map of `func`, in the frame registers LIR
/// emission assigns.
pub(super) fn lir_sidecars(func: &MirFunc) -> LirSidecars {
    let mut plan = EmitPlan::new(func);
    let (regs, _) = assign_needed(func, &mut plan).unwrap_or_else(|_| (Vec::new(), 0));
    let regs = coalesce_latch_overwrite(func, regs, &plan.need_slot);
    (
        super::deopt::debug_slot_remap(func, &regs, &plan.need_slot),
        super::deopt::encode_draft(func, &regs, &plan.need_slot),
    )
}

/// The frame `func`'s LIR keeps its homes past: `(home 0's word, frame
/// words)`, `None` when emission would refuse it. Emission answers it too
/// ([`LirFrame::home_frame`]).
#[cfg(test)]
pub(super) fn lir_home_frame(func: &MirFunc) -> Option<(u8, u32)> {
    let mut plan = EmitPlan::new(func);
    let (regs, scratch) = assign_needed(func, &mut plan).ok()?;
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
    /// Each block's compare fused into its branch ([`super::emit::term_cmp_dests`]).
    cmp_dests: Vec<Option<ValueId>>,
    /// Each block's preds ([`MirFunc::preds`]).
    preds: Vec<Vec<BlockId>>,
    /// Frame word of home 0 ([`MirFunc::homes`]).
    home_base: u8,
    /// A host / FFI / indirect call result left on the stack for the
    /// block's `JumpIfMatch` instead of a store and reload
    /// ([`stack_scrutinees`]).
    stack_scrutinee: Vec<bool>,
    /// Each barrier's paired allocation ([`super::emit::paired_alloc_dests`]).
    paired: HashMap<ValueId, ValueId>,
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
                for v in term_uses(term) {
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
        let cmp_dests = super::emit::term_cmp_dests(func);
        let fused = fused_cmp_dests(func, &cmp_dests);
        let paired = super::emit::paired_alloc_dests(func);
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
                if let Some(d) = cmp_dests[block.id.index()]
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
                    // A barrier emits only its paired allocation's word: any
                    // other root it names is read nowhere, so it keeps its
                    // slot and its own instruction runs where it stands
                    // (a dead `codes.push(x)` result, #1381).
                    let paired = match inst {
                        MirInst::GcBarrier { dest, .. } => Some(paired.get(dest).copied()),
                        _ => None,
                    };
                    // Operands of stored bins can still be tree (i % 10 → BinSlotImm).
                    for v in inst.operands() {
                        if paired.is_some_and(|p| p != Some(v)) {
                            continue;
                        }
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

        let preds = func.preds();
        let stack_scrutinee = stack_scrutinees(func, &preds, &def);
        for (i, on_stack) in stack_scrutinee.iter().enumerate() {
            if *on_stack {
                need_slot[i] = false;
            }
        }

        Self {
            tree,
            need_slot,
            def,
            cmp_dests,
            preds,
            home_base: 0,
            stack_scrutinee,
            paired,
        }
    }

    /// Per value, the op it is emitted at: itself when it stands alone,
    /// the root of its expression tree when it is an inline operand
    /// (a tree value has one user, in its block), `None` when that root
    /// is a terminator's operand.
    fn tree_roots(&self, func: &MirFunc) -> Vec<Option<ValueId>> {
        let n = self.tree.len();
        let mut user: Vec<Option<ValueId>> = vec![None; n];
        for inst in func.blocks.iter().flat_map(|b| &b.insts) {
            if inst.is_phi() || matches!(inst, MirInst::ClearWord { .. }) {
                continue;
            }
            for o in inst.operands() {
                if self.tree.get(o.index()).copied().unwrap_or(false) {
                    user[o.index()] = Some(inst.dest());
                }
            }
        }
        (0..n)
            .map(|i| {
                let mut v = ValueId(i as u32);
                // A tree is a chain of single users: at most n steps.
                for _ in 0..n {
                    if !self.tree[v.index()] {
                        return Some(v);
                    }
                    v = user[v.index()]?;
                }
                None
            })
            .collect()
    }

    /// The frame word of `home`.
    fn home_slot(&self, home: u16) -> u32 {
        u32::from(self.home_base) + u32::from(home)
    }
}

/// Effectful call results (`HostInvoke`, `Ffi`, `CallIndirect`) that only
/// feed their block's `JumpIfMatch`: `x?` on a host call. Such a word goes
/// straight from the call to the match, with no `STORE n; LOAD n` of a slot
/// nothing else reads. A miss leaves it on the stack, so its other uses may
/// only be payload reads in a block reached by that miss alone, made before
/// anything else runs there (the stack word is not in the frame's maps).
fn stack_scrutinees(func: &MirFunc, preds: &[Vec<BlockId>], def: &[Option<(BlockId, usize)>]) -> Vec<bool> {
    let mut out = vec![false; func.types.len()];
    if func.debug_keep {
        return out;
    }
    for block in &func.blocks {
        let Some(Terminator::JumpIfMatch { scrutinee: s, payloads, .. }) = &block.term else {
            continue;
        };
        let Some((db, k)) = def.get(s.index()).copied().flatten() else {
            continue;
        };
        if db != block.id
            || !matches!(
                block.insts[k],
                MirInst::HostInvoke { .. } | MirInst::Ffi { .. } | MirInst::CallIndirect { .. }
            )
            || super::emit::miss_chain_tos(func, &preds[block.id.index()], block.id).is_some()
        {
            continue;
        }
        // Only the match's own payloads may follow the call in its block.
        let tail_ok = block.insts[k + 1..].iter().all(|i| {
            matches!(i, MirInst::MatchPayload { dest, scrutinee: src, .. } if src == s && payloads.contains(dest))
        });
        let uses_ok = func.blocks.iter().filter(|b| b.id != block.id).all(|b| {
            let in_term = b.term.as_ref().is_some_and(|t| term_uses(t).contains(s));
            let mut seen_other = false;
            let mut ok = !in_term;
            for inst in b.insts.iter().filter(|i| !i.is_phi() || i.operands().contains(s)) {
                match inst {
                    MirInst::MatchPayload { scrutinee: src, .. } if src == s => {
                        ok &= !seen_other && super::emit::scrutinee_left_by_miss(func, b.id, *s);
                    }
                    other => {
                        ok &= !other.operands().contains(s);
                        seen_other = true;
                    }
                }
            }
            ok
        });
        if tail_ok && uses_ok {
            out[s.index()] = true;
        }
    }
    out
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
        MirInst::HeapFieldLoad { .. } | MirInst::Index { .. } | MirInst::ArrayLen { .. } | MirInst::DoneCoro { .. } | MirInst::LoadStatic { .. } | MirInst::HomeLoad { .. }
    )
}

/// An inline operand is emitted where its user is, which may reorder it
/// past a memory op between: a read past a write (`let s = o.f; o.f = x;
/// return s;`) or a write past either. Such an operand keeps its slot.
fn keep_memory_order(func: &MirFunc, tree: &mut [bool], fused: &[bool]) {
    let sources = super::emit::CopySources::new(func);
    for block in &func.blocks {
        let insts = &block.insts;
        let at: HashMap<ValueId, usize> = insts.iter().enumerate().map(|(k, i)| (i.dest(), k)).collect();
        // Each clear's zeroed values, once walked.
        let mut walked: HashMap<usize, Vec<ValueId>> = HashMap::new();
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
                for v in term_uses(term) {
                    visit(v, &at, insts, &is_inline, &mut order);
                }
            }
            // A clear zeroes its value's slot where it stands, so an
            // inline user from before the clear must not be emitted after
            // it (`let j = join(s); clear s; return j` would read the
            // zeroed slot): that user keeps its slot. The same holds for
            // the slots of the value's copy sources (`emit::source_clears`).
            // Only a clear with an earlier op emitted after it walks its
            // value's sources: a chain of joins walked each clear's back to
            // its start (#1502).
            let mut later_min = vec![usize::MAX; order.len() + 1];
            for i in (0..order.len()).rev() {
                later_min[i] = later_min[i + 1].min(order[i]);
            }
            let stale = order.iter().enumerate().find_map(|(i, &c)| {
                let MirInst::ClearWord { value, .. } = &insts[c] else {
                    return None;
                };
                if later_min[i + 1] >= c {
                    return None;
                }
                let zeroed = walked.entry(c).or_insert_with(|| {
                    let mut zeroed = sources.of(func, *value, |_| false);
                    zeroed.push(*value);
                    zeroed
                });
                order[i + 1..]
                    .iter()
                    .find(|&&k| k < c && insts[k].operands().iter().any(|o| zeroed.contains(o)))
                    .map(|&k| insts[k].dest())
            });
            if let Some(v) = stale
                && tree[v.index()]
            {
                tree[v.index()] = false;
                continue;
            }
            // An op that can trap counts as a read: a write it is delayed
            // past would run before the panic (#856).
            let mem: Vec<usize> = order
                .into_iter()
                .filter(|&k| reads_memory(&insts[k]) || writes_memory(&insts[k]) || insts[k].may_trap(func))
                .collect();
            // A later-defined op emitted first means an earlier one was
            // delayed past it.
            // The first `a` with such a `b` after it, found from suffix
            // minima so a long block stays linear.
            let writes: Vec<bool> = mem.iter().map(|&k| writes_memory(&insts[k])).collect();
            let mut min_any = vec![usize::MAX; mem.len() + 1];
            let mut min_write = vec![usize::MAX; mem.len() + 1];
            for i in (0..mem.len()).rev() {
                min_any[i] = min_any[i + 1].min(mem[i]);
                min_write[i] = if writes[i] { min_write[i + 1].min(mem[i]) } else { min_write[i + 1] };
            }
            let late = (0..mem.len())
                .find(|&i| min_write[i + 1] < mem[i] || (writes[i] && min_any[i + 1] < mem[i]))
                .and_then(|i| {
                    let a = mem[i];
                    (i + 1..mem.len())
                        .find(|&j| mem[j] < a && (writes[i] || writes[j]))
                        .map(|j| insts[mem[j]].dest())
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
                .chain(block.term.as_ref().map(term_uses));
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

fn fused_cmp_dests(func: &MirFunc, cmp_dests: &[Option<ValueId>]) -> Vec<bool> {
    let mut fused = vec![false; func.types.len()];
    for d in cmp_dests.iter().flatten() {
        fused[d.index()] = true;
    }
    fused
}

/// `JumpIfMatch` leaves the scrutinee (miss) or payloads (taken) on the
/// stack. Store live payload slots; miss keeps TOS for the next match.
fn consume_match_tos(
    out: &mut Vec<IlOp>,
    func: &MirFunc,
    block: BlockId,
    plan: &EmitPlan,
    regs: &[u8],
    loc: DebugLoc,
) {
    for pred in &plan.preds[block.index()] {
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

fn assign_needed(func: &MirFunc, plan: &mut EmitPlan) -> Result<(Vec<u8>, u8), LowerError> {
    match super::emit::assign_regs_prefer_payloads(func, &plan.need_slot) {
        // One register per value overflows the frame (a body with many
        // inlined calls, #1375): share registers between values never live
        // at once. Under a debugger every value keeps its own word.
        Err(LowerError::Refused(why)) if why.starts_with("too many") && !why.contains("params") && !func.debug_keep => {
            // An inline operand reads its own operands where its consumer
            // is: a long chain (`t = t + i` per call) would hold every
            // leaf to the end. Each value with operands takes a slot.
            for i in 0..plan.tree.len() {
                let has_operands = plan.def[i].is_some_and(|(b, k)| !func.block(b).insts[k].operands().is_empty());
                if plan.tree[i] && has_operands {
                    plan.tree[i] = false;
                    plan.need_slot[i] = true;
                }
            }
            assign_shared(func, plan)
        }
        other => other,
    }
}

/// Registers for the slotted values of `func`, shared between values never
/// live at the same time. Parameters keep their own words. A heap word and
/// a scalar never share one (the stack map lists every heap register at
/// every site), and a value's register is held until its last read,
/// counting a `ClearWord` or `Deopt` naming it and a tree operand read
/// where its consumer is emitted.
fn assign_shared(func: &MirFunc, plan: &EmitPlan) -> Result<(Vec<u8>, u8), LowerError> {
    use std::collections::{BTreeSet, HashSet};
    let n = func.types.len();
    let slot = |v: ValueId| plan.need_slot.get(v.index()).copied().unwrap_or(false);
    let fused: HashSet<ValueId> = plan.cmp_dests.iter().flatten().copied().collect();
    // The slotted words reading `v` reads: `v` itself, or the operands of
    // the tree (or fused compare) it is emitted as at its consumer.
    fn reads(func: &MirFunc, plan: &EmitPlan, fused: &HashSet<ValueId>, v: ValueId, out: &mut Vec<ValueId>) {
        if plan.need_slot.get(v.index()).copied().unwrap_or(false) {
            out.push(v);
            return;
        }
        let inline = plan.tree.get(v.index()).copied().unwrap_or(false) || fused.contains(&v);
        if let (true, Some((b, i))) = (inline, plan.def.get(v.index()).copied().flatten()) {
            for o in func.block(b).insts[i].operands() {
                reads(func, plan, fused, o, out);
            }
        }
    }
    let read = |v: ValueId, out: &mut Vec<ValueId>| reads(func, plan, &fused, v, out);
    // Emitted where its consumer is, not at its definition.
    let inline_def = |inst: &MirInst| {
        let d = inst.dest();
        !slot(d) && (plan.tree.get(d.index()).copied().unwrap_or(false) || fused.contains(&d))
    };
    let phi_dests = |b: &super::func::MirBlock| -> Vec<ValueId> {
        b.insts.iter().take_while(|i| i.is_phi()).map(|i| i.dest()).collect()
    };
    let nb = func.blocks.len();
    let mut live_in: Vec<HashSet<ValueId>> = vec![HashSet::new(); nb];
    let live_out = |bi: usize, live_in: &[HashSet<ValueId>]| -> HashSet<ValueId> {
        let mut live = HashSet::new();
        let Some(term) = &func.blocks[bi].term else {
            return live;
        };
        let me = func.blocks[bi].id;
        for s in term.succs() {
            let Some(succ) = func.blocks.get(s.index()) else {
                continue;
            };
            live.extend(live_in[s.index()].iter().copied());
            for inst in succ.insts.iter().take_while(|i| i.is_phi()) {
                if let MirInst::Phi { args, .. } = inst {
                    let mut out = Vec::new();
                    for &(_, v) in args.iter().filter(|(p, _)| *p == me) {
                        read(v, &mut out);
                    }
                    live.extend(out);
                }
            }
        }
        live
    };
    // One backward walk of block `bi`; `edge` records an interference.
    let walk = |bi: usize, live_in: &[HashSet<ValueId>], edge: &mut dyn FnMut(ValueId, ValueId)| -> HashSet<ValueId> {
        let block = &func.blocks[bi];
        let mut live = live_out(bi, live_in);
        let def = |defs: &[ValueId], uses: &[ValueId], live: &mut HashSet<ValueId>, edge: &mut dyn FnMut(ValueId, ValueId)| {
            for &d in defs {
                for &o in live.iter().chain(uses).chain(defs) {
                    if o != d {
                        edge(d, o);
                    }
                }
            }
            for d in defs {
                live.remove(d);
            }
            live.extend(uses.iter().copied());
        };
        if let Some(term) = &block.term {
            let (defs, used): (Vec<ValueId>, Vec<ValueId>) = match term {
                Terminator::JumpIfMatch { scrutinee, payloads, .. } => {
                    (payloads.iter().copied().filter(|&v| slot(v)).collect(), vec![*scrutinee])
                }
                other => (Vec::new(), term_uses(other)),
            };
            let mut uses = Vec::new();
            for v in used {
                read(v, &mut uses);
            }
            def(&defs, &uses, &mut live, edge);
        }
        for inst in block.insts.iter().rev() {
            if inst.is_phi() || inline_def(inst) {
                continue;
            }
            let defs: Vec<ValueId> = inst.dests().filter(|&d| slot(d)).collect();
            let mut uses = Vec::new();
            for o in inst.operands() {
                read(o, &mut uses);
            }
            def(&defs, &uses, &mut live, edge);
        }
        let phis = phi_dests(block);
        for &d in &phis {
            if slot(d) {
                for &o in live.iter().chain(&phis) {
                    if o != d && slot(o) {
                        edge(d, o);
                    }
                }
            }
        }
        for d in &phis {
            live.remove(d);
        }
        live
    };
    let mut changed = true;
    while changed {
        changed = false;
        for bi in (0..nb).rev() {
            let live = walk(bi, &live_in, &mut |_, _| {});
            if live != live_in[bi] {
                live_in[bi] = live;
                changed = true;
            }
        }
    }
    let mut adj: Vec<BTreeSet<ValueId>> = vec![BTreeSet::new(); n];
    {
        let mut edge = |a: ValueId, b: ValueId| {
            if a.index() < n && b.index() < n {
                adj[a.index()].insert(b);
                adj[b.index()].insert(a);
            }
        };
        for bi in 0..nb {
            let _ = walk(bi, &live_in, &mut edge);
        }
        // A φ is written on each incoming edge, over what that edge keeps.
        for block in &func.blocks {
            for inst in block.insts.iter().take_while(|i| i.is_phi()) {
                let MirInst::Phi { dest, args, .. } = inst else { continue };
                if !slot(*dest) {
                    continue;
                }
                for &(pred, _) in args {
                    if pred.index() < nb {
                        for o in live_out(pred.index(), &live_in) {
                            if o != *dest {
                                edge(*dest, o);
                            }
                        }
                    }
                }
            }
        }
    }
    let opaque = super::gc::live_opaque_words(func);
    let heap = |v: ValueId| func.types.get(v.index()).is_some_and(|t| t.is_heap_word()) || opaque.contains(&v);
    let mut reg = vec![0u8; n];
    let mut color: Vec<Option<usize>> = vec![None; n];
    for (i, &p) in func.params.iter().enumerate() {
        reg[p.index()] = i as u8;
    }
    let base = func.params.len();
    // Whether each register holds heap words (`Some(true)`) or scalars.
    let mut class: Vec<Option<bool>> = Vec::new();
    let mut top = base;
    for i in 0..n {
        let v = ValueId(i as u32);
        if !slot(v) || func.params.contains(&v) {
            continue;
        }
        let taken: HashSet<usize> = adj[i].iter().filter_map(|o| color[o.index()]).collect();
        let h = heap(v);
        let c = (base..)
            .find(|c| !taken.contains(c) && class.get(c - base).copied().flatten().is_none_or(|k| k == h))
            .expect("unbounded");
        if c > 254 {
            return Err(LowerError::Refused("too many live slots".into()));
        }
        if class.len() <= c - base {
            class.resize(c - base + 1, None);
        }
        class[c - base] = Some(h);
        color[i] = Some(c);
        reg[i] = c as u8;
        top = top.max(c + 1);
    }
    Ok((reg, top as u8))
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
                    MirInst::Const { c: MirConst::I64(1), .. }
                )
            })
        }),
        _ => false,
    }
}

/// `v` as an `i16` immediate, when it is an inline-tree `Const`.
fn tree_i16(func: &MirFunc, plan: &EmitPlan, v: ValueId) -> Option<i16> {
    if !plan.tree[v.index()] {
        return None;
    }
    const_i16(func, plan.def[v.index()])
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
            push_const::<u32>(out, *c, pool, loc)?;
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
                op: stack_bin_op(*op, *ty)?,
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
                op: stack_cmp_op(*op, *ty)?,
                loc,
            });
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        // Dense-only (`byte_scan`).
        MirInst::ByteScan { .. } => return Err(LowerError::Refused("LIR has no byte lanes".into())),
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
        MirInst::HeapFieldLoad {
            dest,
            object,
            index,
        } => {
            emit_heap_field_load(EmitHeapFieldLoadArgs {
                out,
                object: *object,
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
            index,
        } => {
            emit_heap_field_store(EmitHeapFieldStoreArgs {
                out,
                object: *object,
                value: *value,
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
            if !plan.stack_scrutinee[dest.index()] {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::Ffi { dest, op, operand, args } => {
            // As HIR emission: the operands in order, then the opcode.
            for a in args {
                emit_stack(out, *a, func, plan, regs, pool, loc)?;
            }
            out.push(IlOp::from_plain_byte(Byte::new(op.instruction()).with_operand_u32(*operand), loc));
            if !plan.stack_scrutinee[dest.index()] {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::CallIndirect { dest, callee, args, dicts } => {
            // The arguments, then the function value on top.
            for a in args.iter().chain([callee]) {
                emit_stack(out, *a, func, plan, regs, pool, loc)?;
            }
            let operand = super::emit::call_indirect_operand(args.len(), *dicts);
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::CallIndirect).with_operand_u32(operand), loc));
            if !plan.stack_scrutinee[dest.index()] {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
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
            if let Some(&obj) = plan.paired.get(dest) {
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
        ret_words: crate::layout::ret_words_from_hi(dest_hi),
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
            op: stack_bin_op(*op, *ty)?,
            loc,
        });
        return Ok(());
    }
    emit_stack(out, hi, func, plan, regs, pool, loc)
}

struct EmitHeapFieldLoadArgs<'args> {
    out: &'args mut Vec<IlOp>,
    object: ValueId,
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
        index,
        func,
        plan,
        regs,
        pool,
        loc,
    } = args;

    emit_stack(out, object, func, plan, regs, pool, loc)?;
    out.push(IlOp::LoadField { index, loc });
    Ok(())
}

struct EmitHeapFieldStoreArgs<'args> {
    out: &'args mut Vec<IlOp>,
    object: ValueId,
    value: ValueId,
    index: u32,
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
        index,
        func,
        plan,
        regs,
        pool,
        loc,
    } = args;

    emit_stack(out, value, func, plan, regs, pool, loc)?;
    emit_stack(out, object, func, plan, regs, pool, loc)?;
    out.push(IlOp::SetField { index: Some(index), loc });
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
        MirInst::Const { c, .. } => push_const::<u32>(out, *c, pool, loc),
        MirInst::Bin {
            op, ty, lhs, rhs, ..
        } => emit_bin(EmitBinArgs {
            out,
            op: stack_bin_op(*op, *ty)?,
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
            op: stack_cmp_op(*op, *ty)?,
            lhs: *lhs,
            rhs: *rhs,
            func,
            plan,
            regs,
            pool,
            loc,
        }),
        MirInst::ByteScan { .. } => Err(LowerError::Refused("LIR has no byte lanes".into())),
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
        MirInst::HeapFieldLoad {
            object,
            index,
            ..
        } => emit_heap_field_load(EmitHeapFieldLoadArgs {
            out,
            object: *object,
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
            index,
            ..
        } => emit_heap_field_store(EmitHeapFieldStoreArgs {
            out,
            object: *object,
            value: *value,
            index: *index,
            func,
            plan,
            regs,
            pool,
            loc,
        }),
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
            if let Some(&obj) = plan.paired.get(dest) {
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
    /// Registers zeroed after an edge's φ copies (`emit::entry_clears`).
    clears: &'args HashMap<(BlockId, BlockId), Vec<u8>>,
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
        clears,
    } = args;

    let Some(term) = &block.term else {
        return Err(LowerError::Refused("missing terminator".into()));
    };
    let clears = |to: BlockId| clears.get(&(block.id, to)).map(Vec::as_slice).unwrap_or(&[]);
    match term {
        Terminator::Jump { dest } => {
            emit_phi_moves(out, func, block.id, *dest, regs, scratch, loc);
            for &r in clears(*dest) {
                super::emit::push_clear_word(out, r);
            }
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
            let (t_clears, f_clears) = (clears(*taken), clears(*not_taken));
            let (t_edge, f_edge) = (!t_moves.is_empty() || !t_clears.is_empty(), !f_moves.is_empty() || !f_clears.is_empty());
            if !t_edge && !f_edge {
                emit_cond_jumps(out, func, block.id, *taken, *not_taken, block_lab, loc);
            } else if !f_edge {
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
                for &r in t_clears {
                    super::emit::push_clear_word(out, r);
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
                for &r in t_clears {
                    super::emit::push_clear_word(out, r);
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
                for &r in f_clears {
                    super::emit::push_clear_word(out, r);
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
                ret_words: crate::layout::ret_words_from_hi(*dest_hi),
            });
        }
        Terminator::Return { lo, hi } => {
            let ret_words = crate::layout::ret_words_from_hi(*hi);
            if crate::layout::is_multi_word_ret(ret_words) && func.ret_layout != MirLayout::TwoSlot {
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
            if super::emit::miss_chain_tos(func, &plan.preds[block.id.index()], block.id).is_none()
                && !plan.stack_scrutinee[scrutinee.index()]
            {
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
            if super::emit::miss_edge_pops(func, *scrutinee, *not_taken) {
                out.push(IlOp::Pop { loc });
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
            op: stack_cmp_op(*op, *ty)?,
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
