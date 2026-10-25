//! Lower verified numeric MIR to dense bytecode (`IlOp` residuals + labels).

use std::collections::{HashMap, HashSet};

use common::{dense, Byte, DebugLoc, Instruction};

use crate::il::{IlJumpKind, IlOp, Label};

use super::call_convoy::{is_tail_call_inst, ConvoyPlan};
use super::destprop::sink_phi_incomings;
use super::emit_common::{
    const_i16, intern_pool, is_jim_term_payload, last_arm_payloads, phi_moves, push_const,
    stack_bin_op, stack_cmp_op,
};
use super::func::MirFunc;
use super::inst::{
    BlockId, MirAllocKind, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, MirUnaryOp,
    Terminator, ValueId,
};
use super::error::LowerError;
use super::ty::MirTy;

/// Emit dense IL for `func`. Preserves `entry_label` so CALL targets stay valid.
///
/// `across_alloc` is S2c: emit `Make*` / `InitTyped` residuals at mapped
/// GC edges. Default (`false`) still refuses.
pub fn emit_dense(
    func: &MirFunc,
    entry_label: Option<Label>,
    pool: &mut Vec<u64>,
    across_alloc: bool,
) -> Result<Vec<IlOp>, LowerError> {
    if func.has_gc_edge() && !across_alloc {
        return Err(LowerError::Refused(
            "dense emit refuses Alloc/GcBarrier without GC maps (S2c)".into(),
        ));
    }
    if func.blocks.iter().any(|b| {
        b.insts.iter().any(|i| match i {
            MirInst::HostInvoke { native_id, .. } => {
                !super::host_allow::dense_host_ok(*native_id)
            }
            MirInst::Ffi { .. } => true,
            _ => false,
        })
    }) {
        return Err(LowerError::Refused(
            "dense emit refuses untyped HostInvoke".into(),
        ));
    }
    let mut func = func.clone();
    sink_phi_incomings(&mut func);
    let func = &func;
    let plan = ConvoyPlan::new(func, entry_label);
    let (regs, scratch) = assign_regs(func, &plan.need_slot)?;
    let regs = coalesce_latch_overwrite(func, regs, &plan.need_slot);
    let regs = alias_dead_store_dests(func, regs);
    let mut cleared = cleared_words(func, &regs, &plan.need_slot);
    let mut entry_clears = entry_clears(func, &regs, &plan.need_slot);
    let gathers = Gathers::new(func, &regs, &plan.need_slot, scratch, entry_label);
    let mut source_clears = source_clears(func, &regs, &plan.need_slot, &gathers);
    prune_clears(func, &regs, &plan.need_slot, &mut cleared, &mut entry_clears, &mut source_clears, &gathers.writes);
    let arg_clears = arg_clears(func, &regs, &plan.need_slot, &gathers, Some);
    let gather = gather_window(func, entry_label);
    let mut max_slot = plan
        .need_slot
        .iter()
        .enumerate()
        .filter(|(_, n)| **n)
        .map(|(i, _)| regs[i])
        .max()
        .unwrap_or(0);
    if gather > 0 {
        max_slot = max_slot.max(scratch.saturating_add(gather.saturating_sub(1)));
    }
    // Sibling / mutual CALL targets keep their official entry ids. Local
    // SSA labels must not reuse those ids or to_flat treats the TailCall
    // as intra-body (B2 even/odd break).
    let reserved = foreign_targets(func, entry_label);
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
    let frame = u32::from(max_slot) + 1;
    if frame > func.params.len() as u32 {
        out.push(IlOp::byte(
            Byte::new(Instruction::Seek).with_operand_u32(frame),
        ));
    }

    let cmp_dests = term_cmp_dests(func);
    let all_preds = func.preds();
    let rotate = rotatable_headers(func, &cmp_dests, &all_preds, &arg_clears, &entry_clears, regs.as_slice(), scratch);
    for block in &func.blocks {
        let preds = &all_preds[block.id.index()];
        if block.id != func.entry {
            out.push(IlOp::Label(block_lab[block.id.index()]));
        }
        for &r in arg_clears.entry(block.id) {
            push_clear_word(&mut out, r);
        }
        let mut stacked: Vec<ValueId> = Vec::new();
        let mut starts: Vec<(usize, ValueId)> = Vec::new();
        let loc = func.term_loc(block.id);
        seed_jim_taken_stack(&mut out, &mut stacked, func, block.id, preds, &plan, &regs, loc);
        if let Some(s) = miss_chain_tos(func, preds, block.id) {
            // The next arm's test peeks the scrutinee the miss left.
            stacked.push(s);
        } else if missed_scrutinee_unused(func, preds, block.id) {
            out.push(IlOp::Pop { loc });
        }
        // The zeros [`arg_clears`] puts after the op emitted last.
        let mut after: &[u8] = &[];
        for inst in &block.insts {
            for &r in std::mem::replace(&mut after, arg_clears.after(inst.dest())) {
                push_clear_word(&mut out, r);
            }
            if inst.is_phi() {
                continue;
            }
            if cmp_dests[block.id.index()].is_some_and(|d| {
                matches!(inst, MirInst::Cmp { dest, .. } if *dest == d)
            }) {
                continue;
            }
            if is_tail_call_inst(func, block, inst) {
                continue;
            }
            starts.push((out.len(), inst.dest()));
            let loc = func.loc_of(inst.dest());
            if let MirInst::ClearWord { dest, value } = inst {
                if cleared.contains(dest) {
                    push_clear_word(&mut out, regs[value.index()]);
                }
                for &r in source_clears.get(dest).map(Vec::as_slice).unwrap_or(&[]) {
                    push_clear_word(&mut out, r);
                }
                continue;
            }
            if let MirInst::KeepAlive { .. } = inst {
                continue;
            }
            if let MirInst::Call {
                dest,
                dest_hi,
                target,
                args,
            } = inst
            {
                emit_call(EmitCallArgs {
                    out: &mut out,
                    stacked: &mut stacked,
                    kind: crate::il::EntryKind::Call,
                    dest: *dest,
                    dest_hi: *dest_hi,
                    target: *target,
                    args,
                    func,
                    plan: &plan,
                    regs: &regs,
                    scratch,
                    pool,
                    loc,
                })?;
                continue;
            }
            if let MirInst::MatchPayload {
                dest,
                scrutinee,
                index,
            } = inst
            {
                if is_jim_term_payload(func, *dest) {
                    continue;
                }
                let st = func.ty(*scrutinee);
                if matches!(st, MirTy::NicheOpt | MirTy::NicheRes) {
                    if plan.needs_slot(*dest) {
                        emit_inst(EmitInstArgs {
                            out: &mut out,
                            inst,
                            func,
                            regs: &regs,
                            scratch,
                            pool,
                            loc,
                            across_alloc,
                        })?;
                        stacked.clear();
                    }
                    continue;
                }
                if *index == 0 {
                    emit_boxed_last_arm_unpack(EmitBoxedLastArmUnpackArgs {
                        out: &mut out,
                        stacked: &mut stacked,
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
            if !plan.needs_slot(inst.dest()) {
                continue;
            }
            // Register form needs every operand in a register. A convoyed
            // operand (a self-call result on the stack) feeding a stored
            // value goes through the stack instead.
            if inst.operands().iter().any(|v| !plan.needs_slot(*v)) {
                let MirInst::Bin {
                    dest,
                    op,
                    ty,
                    lhs,
                    rhs,
                } = inst
                else {
                    return Err(LowerError::Refused(
                        "dense operand without a register".into(),
                    ));
                };
                emit_stack_bin(EmitStackBinArgs {
                    out: &mut out,
                    stacked: &mut stacked,
                    op: *op,
                    ty: *ty,
                    lhs: *lhs,
                    rhs: *rhs,
                    dest: *dest,
                    func,
                    plan: &plan,
                    regs: &regs,
                    pool,
                    loc,
                })?;
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
                stacked.clear();
                continue;
            }
            emit_inst(EmitInstArgs {
                out: &mut out,
                inst,
                func,
                regs: &regs,
                scratch,
                pool,
                loc,
                across_alloc,
            })?;
            stacked.clear();
        }
        for &r in after {
            push_clear_word(&mut out, r);
        }
        let end = out.len();
        restore_block_op_locs(&mut out, &starts, end, func);
        emit_term(EmitTermArgs {
            out: &mut out,
            stacked: &mut stacked,
            block,
            func,
            plan: &plan,
            regs: &regs,
            scratch,
            block_lab: &block_lab,
            next_label: &mut next_label,
            reserved: &reserved,
            pool,
            loc: func.term_loc(block.id),
            clears: &entry_clears,
            rotate: &rotate,
        })?;
    }
    coalesce_dense_moves(&mut out);
    pack_chained_dense_bin(&mut out);
    pack_dense_bin_jmpf(&mut out);
    pack_dense_index_jmpf(&mut out);
    Ok(out)
}

/// Named-let remap and deopt draft, after the same register assign as emit.
pub(super) fn dense_sidecars(func: &MirFunc, entry_label: Option<Label>) -> super::emit_lir::LirSidecars {
    let mut func = func.clone();
    sink_phi_incomings(&mut func);
    let plan = ConvoyPlan::new(&func, entry_label);
    let (regs, _) = assign_regs(&func, &plan.need_slot).unwrap_or_else(|_| (Vec::new(), 0));
    let regs = coalesce_latch_overwrite(&func, regs, &plan.need_slot);
    let regs = alias_dead_store_dests(&func, regs);
    (
        super::deopt::debug_slot_remap(&func, &regs, &plan.need_slot),
        super::deopt::encode_draft(&func, &regs, &plan.need_slot),
    )
}

/// Labels of other functions this body calls or starts. A forward callee's
/// entry can be any id above this body's own, so block labels skip them.
pub(super) fn foreign_targets(func: &MirFunc, entry_label: Option<Label>) -> HashSet<u32> {
    func.blocks
        .iter()
        .flat_map(|b| b.insts.iter())
        .filter_map(|inst| match inst {
            MirInst::Call { target, .. }
            | MirInst::CodePtr { target, .. }
            | MirInst::Alloc {
                kind: MirAllocKind::Coro { target } | MirAllocKind::Fn { target, .. } | MirAllocKind::PolyFn { target } | MirAllocKind::PolyFnCapture { target, .. },
                ..
            } if Some(*target) != entry_label => Some(target.0),
            _ => None,
        })
        .collect()
}

pub(super) fn take_label(next: &mut u32, reserved: &HashSet<u32>) -> Label {
    while reserved.contains(next) {
        *next = next.saturating_add(1);
    }
    let id = *next;
    *next = next.saturating_add(1);
    Label(id)
}

pub(super) fn max_label_hint(entry: Option<Label>) -> u32 {
    entry.map(|Label(id)| id.saturating_add(1)).unwrap_or(1)
}

/// The `ClearWord`s whose register may be zeroed: their word has one, and
/// no value in that register is live after them (another local holding
/// the same value, or a value coalesced into it).
pub(super) fn cleared_words(func: &MirFunc, regs: &[u8], need_slot: &[bool]) -> HashSet<ValueId> {
    let mut out = HashSet::new();
    if !func.blocks.iter().any(|b| b.insts.iter().any(|i| matches!(i, MirInst::ClearWord { .. }))) {
        return out;
    }
    let live = super::gc::liveness_after(func);
    let cmp_dests = term_cmp_dests(func);
    for (b, block) in func.blocks.iter().enumerate() {
        // A compare fused into the branch is emitted at the terminator,
        // after any clear in the block: keep the words it reads.
        let fused_reads: Vec<ValueId> = cmp_dests[b]
            .and_then(|d| block.insts.iter().find(|i| i.dest() == d))
            .map(|i| i.operands())
            .unwrap_or_default();
        for (k, inst) in block.insts.iter().enumerate() {
            let MirInst::ClearWord { dest, value } = inst else {
                continue;
            };
            let slot = |v: &ValueId| need_slot.get(v.index()).copied().unwrap_or(false);
            if slot(value)
                && !fused_reads.contains(value)
                && !func.params.contains(value)
                && !live[b][k].iter().any(|v| slot(v) && regs[v.index()] == regs[value.index()])
                && !copied_over_first(func, block, k, regs, regs[value.index()], &slot)
            {
                out.insert(*dest);
            }
        }
    }
    out
}

/// The block's edge copy rewrites register `reg` after instruction `k` with
/// no collection possible in between, so a zero there is dead: a loop local
/// rebound in the body, whose header φ shares `reg` (#1402).
fn copied_over_first(
    func: &MirFunc,
    block: &super::func::MirBlock,
    k: usize,
    regs: &[u8],
    reg: u8,
    slot: &impl Fn(&ValueId) -> bool,
) -> bool {
    let Some(Terminator::Jump { dest: succ }) = block.term else {
        return false;
    };
    // Ops that cannot collect, nor trap (a trap may unwind into a cleanup
    // pad that collects).
    let quiet = |inst: &MirInst| {
        (!super::gc::collects(inst) || matches!(inst, MirInst::ClearWord { .. } | MirInst::KeepAlive { .. })) && !inst.may_trap(func)
    };
    if !block.insts[k + 1..].iter().all(quiet) {
        return false;
    }
    func.block(succ)
        .insts
        .iter()
        .map_while(|inst| match inst {
            MirInst::Phi { dest, args, .. } => Some((dest, args)),
            _ => None,
        })
        .any(|(dest, args)| {
            slot(dest) && regs[dest.index()] == reg && args.iter().any(|&(p, src)| p == block.id && regs[src.index()] != reg)
        })
}

/// Registers to zero after the φ copies of each forward edge (by
/// predecessor and target): a heap word copied into a φ that is later rebound
/// (a `ClearWord` of the φ, or of a φ it flows into) and nothing reads past
/// the edge. The copy carries the value on; its source register would keep
/// rooting it after the rebind: a loop header's entry copy (#1402), or a
/// branch's copy into an `if`/`else` join (#1409), or a skipped arm's
/// branch copy into the join after it (#1519: zeroed there once, a chain's
/// earlier registers need no zero again at each later arm). Never on a back
/// edge.
pub(super) fn entry_clears(func: &MirFunc, regs: &[u8], need_slot: &[bool]) -> HashMap<(BlockId, BlockId), Vec<u8>> {
    let mut out: HashMap<(BlockId, BlockId), Vec<u8>> = HashMap::new();
    let mut rebound: HashSet<ValueId> = func
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter_map(|i| match i {
            MirInst::ClearWord { value, .. } => Some(*value),
            _ => None,
        })
        .collect();
    if rebound.is_empty() {
        return out;
    }
    // A φ copied into a rebound φ ends with it: a nested join's value.
    let all_phis: Vec<(ValueId, &Vec<(BlockId, ValueId)>)> = func
        .blocks
        .iter()
        .flat_map(|b| b.insts.iter().map_while(|i| match i {
            MirInst::Phi { dest, args, .. } => Some((*dest, args)),
            _ => None,
        }))
        .collect();
    let phi_dests: HashSet<ValueId> = all_phis.iter().map(|(d, _)| *d).collect();
    loop {
        let before = rebound.len();
        for (dest, args) in &all_phis {
            if rebound.contains(dest) {
                for &(_, src) in args.iter() {
                    if phi_dests.contains(&src) {
                        rebound.insert(src);
                    }
                }
            }
        }
        if rebound.len() == before {
            break;
        }
    }
    // A constant (a `None` niche, null), or a cast of one, roots nothing.
    let mut consts: HashSet<ValueId> = HashSet::new();
    let mut casts: HashMap<ValueId, ValueId> = HashMap::new();
    for inst in func.blocks.iter().flat_map(|b| &b.insts) {
        match inst {
            MirInst::Const { dest, .. } => {
                consts.insert(*dest);
            }
            MirInst::Cast { dest, src, .. } => {
                if consts.contains(src) {
                    consts.insert(*dest);
                }
                casts.insert(*dest, *src);
            }
            _ => {}
        }
    }
    let slot = |v: ValueId| need_slot.get(v.index()).copied().unwrap_or(false) && v.index() < regs.len();
    // A cast (an `Option::Some` wrap) of a word another register holds:
    // zeroing this register alone would leave the word rooted there.
    let held_elsewhere = |v: ValueId| {
        let mut cur = v;
        while let Some(&s) = casts.get(&cur) {
            if slot(s) && regs[s.index()] != regs[v.index()] {
                return true;
            }
            cur = s;
        }
        false
    };
    // Loop headers and joins: every block that opens with a φ of a rebound value.
    let headers: Vec<&super::func::MirBlock> = func
        .blocks
        .iter()
        .filter(|h| {
            h.insts
                .iter()
                .take_while(|i| i.is_phi())
                .any(|i| matches!(i, MirInst::Phi { dest, .. } if rebound.contains(dest)))
        })
        .collect();
    if headers.is_empty() {
        return out;
    }
    let live = super::gc::liveness_after(func);
    let sources = CopySources::new(func);
    let below = SourceRegs::new(func, &sources, regs, &slot);
    let mut scan = RewriteScan::new(func, regs, &slot, &[]);
    for h in headers {
        let phis: Vec<(ValueId, &Vec<(BlockId, ValueId)>)> = h
            .insts
            .iter()
            .map_while(|i| match i {
                MirInst::Phi { dest, args, .. } => Some((*dest, args)),
                _ => None,
            })
            .collect();
        // Live at the top of the header: the φs and everything live through.
        let live_in = &live[h.id.index()][phis.len() - 1];
        let taken = |r: u8| {
            phis.iter().any(|(d, _)| regs[d.index()] == r) || live_in.iter().any(|v| slot(*v) && regs[v.index()] == r)
        };
        let preds: HashSet<BlockId> = phis.iter().flat_map(|(_, a)| a.iter().map(|(p, _)| *p)).collect();
        for p in preds {
            // A branch whose one edge reaches the header zeroes its own
            // copies' sources on that edge.
            let jump = matches!(func.block(p).term, Some(Terminator::Jump { dest }) if dest == h.id);
            let br = matches!(func.block(p).term, Some(Terminator::Br { taken, not_taken, .. }) if (taken == h.id) != (not_taken == h.id));
            if p.index() >= h.id.index() || !(jump || br) {
                continue;
            }
            let e = (p, h.id);
            for (dest, args) in &phis {
                for &(q, src) in args.iter() {
                    let r = regs[src.index()];
                    if q == p
                        && rebound.contains(dest)
                        && !consts.contains(&src)
                        && slot(src)
                        && slot(*dest)
                        && func.ty(src).is_heap_word()
                        && !func.params.contains(&src)
                        && !taken(r)
                        && !out.get(&e).is_some_and(|rs| rs.contains(&r))
                        && !held_elsewhere(src)
                        && scan.from(h.id, 0, r)
                    {
                        out.entry(e).or_default().push(r);
                    }
                }
            }
            if !jump {
                continue;
            }
            // The registers another forward predecessor's copy came from
            // still hold what that edge carried last (#1411): in a loop
            // that took the other arm before, the `Some` wrap and the
            // object it was cast from, now rebound away. Only this edge
            // leaves them stale.
            for (dest, args) in &phis {
                if !rebound.contains(dest) || !slot(*dest) {
                    continue;
                }
                let mut own = RegSet::default();
                for &(_, src) in args.iter().filter(|&&(q, _)| q == p) {
                    if slot(src) {
                        own.insert(regs[src.index()]);
                    }
                    own.union(below.of(src));
                }
                // Whether a register is zeroed here or not depends on the
                // register alone: a long chain of sources is asked once
                // per register (#1493), and walked only as far as it still
                // holds a register not asked (#1502).
                let mut asked = RegSet::default();
                let reg = |v: ValueId| (slot(v) && !func.params.contains(&v)).then(|| regs[v.index()]);
                // A back edge's sources are not written yet on loop entry.
                for &(q, src) in args.iter() {
                    if q == p || q.index() >= h.id.index() || consts.contains(&src) || !func.ty(src).is_heap_word() {
                        continue;
                    }
                    let mut new = Vec::new();
                    if let Some(r) = reg(src)
                        && asked.insert(r)
                    {
                        new.push(src);
                    }
                    let mut new: Vec<u8> = new.iter().map(|v| regs[v.index()]).collect();
                    match below.first(src, live_in) {
                        Some(first) => new.extend(first.iter().copied().filter(|&r| asked.insert(r))),
                        None => new.extend(
                            sources
                                .new_regs_along(func, src, |x| live_in.contains(&x), |_, _| true, &below, reg, &mut asked)
                                .iter()
                                .map(|v| regs[v.index()]),
                        ),
                    }
                    for r in new {
                        if !own.contains(r)
                            && !taken(r)
                            && !out.get(&e).is_some_and(|rs| rs.contains(&r))
                            && scan.from(h.id, 0, r)
                        {
                            out.entry(e).or_default().push(r);
                        }
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
thread_local! {
    /// Blocks scanned answering [`RewriteScan`] queries, for the work
    /// bound of #1493.
    pub(super) static REWRITE_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Whether a collection can run, from a point of a block, while a register
/// still holds what an edge left in it: on some path an op that may collect
/// (or, in a body with a cleanup pad, trap) comes before an op that rewrites
/// the register or the frame's exit. Otherwise a zero of the register there
/// is dead (a hot loop's join that allocates nothing).
///
/// Answered from the top of every block at once, per register asked about:
/// one pass over the body and its edges, not a walk of every block a query
/// reaches. A chain of joins asked once per copy source each took ~n³
/// block scans (#1493).
struct RewriteScan<'f> {
    func: &'f MirFunc,
    /// Per block, the ops (past its φs) that may collect, by index.
    loud: Vec<Vec<usize>>,
    /// Per block, the ops (past its φs) whose dest has a register, by index,
    /// and whether the op writes it before it runs (a push's gather
    /// register, [`Gathers`]) rather than as its result.
    writes: Vec<Vec<(usize, u8, bool)>>,
    preds: Vec<Vec<usize>>,
    /// Per register asked, whether a collection may come first from the
    /// top of each block.
    from_top: HashMap<u8, Vec<bool>>,
}

impl<'f> RewriteScan<'f> {
    fn new(func: &'f MirFunc, regs: &[u8], slot: &impl Fn(ValueId) -> bool, gather_writes: &[(usize, usize, u8)]) -> Self {
        let quiet = |inst: &MirInst| {
            (!super::gc::collects(inst) || matches!(inst, MirInst::ClearWord { .. } | MirInst::KeepAlive { .. }))
                && (func.cleanup.is_none() || !inst.may_trap(func))
        };
        let nb = func.blocks.len();
        let mut loud = vec![Vec::new(); nb];
        let mut writes = vec![Vec::new(); nb];
        let mut preds = vec![Vec::new(); nb];
        for (b, block) in func.blocks.iter().enumerate() {
            for (k, inst) in block.insts.iter().enumerate() {
                if inst.is_phi() {
                    continue;
                }
                if !quiet(inst) {
                    loud[b].push(k);
                }
                for &(_, _, r) in gather_writes.iter().filter(|&&(gb, gk, _)| gb == b && gk == k) {
                    writes[b].push((k, r, true));
                }
                let d = inst.dest();
                if slot(d) {
                    writes[b].push((k, regs[d.index()], false));
                }
            }
            for s in block.term.as_ref().map(Terminator::succs).unwrap_or_default() {
                if s.index() < nb {
                    preds[s.index()].push(b);
                }
            }
        }
        Self { func, loud, writes, preds, from_top: HashMap::new() }
    }

    /// Block `b` alone, from its op `start`: `Some(true)` when an op that
    /// may collect comes first (or the block ends the frame), `Some(false)`
    /// when a rewrite of `reg` does, `None` when it hands on to its
    /// successors.
    fn local(&self, b: usize, start: usize, reg: u8) -> Option<bool> {
        #[cfg(test)]
        REWRITE_SCANS.with(|c| c.set(c.get() + 1));
        let loud = self.loud[b].iter().copied().find(|&k| k >= start);
        let write = self.writes[b].iter().find(|&&(k, r, _)| k >= start && r == reg).map(|&(k, _, before)| (k, before));
        match (loud, write) {
            (Some(l), w) if w.is_none_or(|(w, before)| l < w || (l == w && !before)) => Some(true),
            (_, Some(_)) => Some(false),
            _ => self.func.blocks[b].term.is_none().then_some(true),
        }
    }

    /// [`Self::from`] at the top of every block: those that may collect
    /// first themselves, then back through every block that hands on to
    /// one.
    fn tops(&mut self, reg: u8) -> &[bool] {
        if !self.from_top.contains_key(&reg) {
            let nb = self.func.blocks.len();
            let local: Vec<Option<bool>> = (0..nb).map(|b| self.local(b, 0, reg)).collect();
            let mut out: Vec<bool> = local.iter().map(|l| *l == Some(true)).collect();
            let mut work: Vec<usize> = (0..nb).filter(|&b| out[b]).collect();
            while let Some(b) = work.pop() {
                for &p in &self.preds[b] {
                    if !out[p] && local[p].is_none() {
                        out[p] = true;
                        work.push(p);
                    }
                }
            }
            self.from_top.insert(reg, out);
        }
        &self.from_top[&reg]
    }

    /// From op `start` of block `from`.
    fn from(&mut self, from: BlockId, start: usize, reg: u8) -> bool {
        let b = from.index();
        if start == 0 {
            return self.tops(reg)[b];
        }
        if let Some(first) = self.local(b, start, reg) {
            return first;
        }
        let succs = self.func.blocks[b].term.as_ref().map(Terminator::succs).unwrap_or_default();
        let tops = self.tops(reg);
        succs.iter().any(|s| tops.get(s.index()).copied().unwrap_or(false))
    }
}

#[cfg(test)]
thread_local! {
    /// Values [`CopySources`] stepped through, for the work bound of #1502.
    pub(super) static COPY_SOURCE_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Where a value's word was copied from: a cast's source (an
/// `Option::Some` wrap) and a φ's incoming values, transitively. Each keeps
/// the word in its own register after its last use, so once the value is
/// rebound they still root it until rewritten (#1411).
pub(super) struct CopySources<'f> {
    /// Casts and φs, with the block that defines them.
    defs: HashMap<ValueId, (BlockId, &'f MirInst)>,
    /// Constants and casts of them (a `None` niche): they root nothing.
    consts: HashSet<ValueId>,
}

impl<'f> CopySources<'f> {
    pub(super) fn new(func: &'f MirFunc) -> Self {
        let mut defs = HashMap::new();
        let mut consts = HashSet::new();
        for (b, inst) in func.blocks.iter().flat_map(|b| b.insts.iter().map(move |i| (b.id, i))) {
            match inst {
                MirInst::Const { dest, .. } => {
                    consts.insert(*dest);
                }
                MirInst::Cast { dest, src, .. } => {
                    if consts.contains(src) {
                        consts.insert(*dest);
                    }
                    defs.insert(*dest, (b, inst));
                }
                MirInst::Phi { dest, .. } => {
                    defs.insert(*dest, (b, inst));
                }
                _ => {}
            }
        }
        Self { defs, consts }
    }

    /// The copy sources of `v` that may hold a heap word, not following
    /// past a `stop` value (a live value: its own sources may hold its
    /// current word). `v` itself is not listed.
    pub(super) fn of(&self, func: &MirFunc, v: ValueId, stop: impl Fn(ValueId) -> bool) -> Vec<ValueId> {
        self.of_along(func, v, stop, |_, _| true)
    }

    /// [`Self::of`], following a φ's incoming value only along the edges
    /// (φ block, predecessor) that `edge` accepts.
    pub(super) fn of_along(
        &self,
        func: &MirFunc,
        v: ValueId,
        stop: impl Fn(ValueId) -> bool,
        edge: impl Fn(BlockId, BlockId) -> bool,
    ) -> Vec<ValueId> {
        let mut seen: HashSet<ValueId> = HashSet::from([v]);
        let mut out = Vec::new();
        let mut work = vec![v];
        while let Some(x) = work.pop() {
            #[cfg(test)]
            COPY_SOURCE_STEPS.with(|c| c.set(c.get() + 1));
            let srcs: Vec<ValueId> = match self.defs.get(&x) {
                Some((_, MirInst::Cast { src, .. })) => vec![*src],
                Some((h, MirInst::Phi { args, .. })) => args.iter().filter(|&&(p, _)| edge(*h, p)).map(|&(_, a)| a).collect(),
                _ => continue,
            };
            for s in srcs {
                if self.consts.contains(&s) || !func.ty(s).is_heap_word() || stop(s) || !seen.insert(s) {
                    continue;
                }
                out.push(s);
                work.push(s);
            }
        }
        out
    }
}

impl CopySources<'_> {
    /// A value's direct copy sources that [`Self::of_along`] may step to
    /// (whatever `stop` and `edge` say): no constant, only heap words.
    fn direct<'a>(&'a self, func: &'a MirFunc, x: ValueId) -> impl Iterator<Item = (Option<(BlockId, BlockId)>, ValueId)> + 'a {
        let (cast, phi) = match self.defs.get(&x) {
            Some((_, MirInst::Cast { src, .. })) => (Some(*src), None),
            Some((h, MirInst::Phi { args, .. })) => (None, Some((*h, args))),
            _ => (None, None),
        };
        cast.map(|s| (None, s))
            .into_iter()
            .chain(phi.into_iter().flat_map(|(h, args)| args.iter().map(move |&(p, a)| (Some((h, p)), a))))
            .filter(|&(_, s)| !self.consts.contains(&s) && func.ty(s).is_heap_word())
    }

    /// The copy sources of `v` ([`Self::of_along`]) that `reg` gives a
    /// register not in `asked`: the first of each such register in the
    /// order [`Self::of_along`] lists them, each marked asked. A source
    /// whose own sources hold only asked registers ([`SourceRegs`]) is not
    /// followed: nothing past it is new. A chain of n joins is then walked
    /// about as far as its registers differ, not back to its start: once
    /// per join, the full walks were ~n² (#1502).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new_regs_along(
        &self,
        func: &MirFunc,
        v: ValueId,
        stop: impl Fn(ValueId) -> bool,
        edge: impl Fn(BlockId, BlockId) -> bool,
        below: &SourceRegs,
        reg: impl Fn(ValueId) -> Option<u8>,
        asked: &mut RegSet,
    ) -> Vec<ValueId> {
        let mut seen: HashSet<ValueId> = HashSet::from([v]);
        let mut out = Vec::new();
        let mut work = vec![v];
        while let Some(x) = work.pop() {
            #[cfg(test)]
            COPY_SOURCE_STEPS.with(|c| c.set(c.get() + 1));
            if asked.contains_all(below.of(x)) {
                continue;
            }
            let srcs: Vec<ValueId> = self.direct(func, x).filter(|&(e, _)| e.is_none_or(|(h, p)| edge(h, p))).map(|(_, s)| s).collect();
            for s in srcs {
                if stop(s) || !seen.insert(s) {
                    continue;
                }
                if let Some(r) = reg(s)
                    && asked.insert(r)
                {
                    out.push(s);
                }
                work.push(s);
            }
        }
        out
    }
}

/// A set of frame registers.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct RegSet([u64; 4]);

impl RegSet {
    /// Adds `r`; whether it was new.
    pub(super) fn insert(&mut self, r: u8) -> bool {
        let (w, bit) = (usize::from(r) / 64, 1u64 << (r % 64));
        let new = self.0[w] & bit == 0;
        self.0[w] |= bit;
        new
    }

    pub(super) fn contains(&self, r: u8) -> bool {
        self.0[usize::from(r) / 64] & (1u64 << (r % 64)) != 0
    }

    pub(super) fn union(&mut self, other: &RegSet) {
        for (a, b) in self.0.iter_mut().zip(other.0) {
            *a |= b;
        }
    }

    pub(super) fn remove(&mut self, r: u8) {
        self.0[usize::from(r) / 64] &= !(1u64 << (r % 64));
    }

    fn contains_all(&self, other: &RegSet) -> bool {
        self.0.iter().zip(other.0).all(|(a, b)| b & !a == 0)
    }
}

/// Per value, the registers of all its copy sources ([`CopySources`]) that
/// have a slot, along every edge and past every value: what any walk of
/// its sources can meet, at most. And, where the sources form no cycle,
/// the registers [`CopySources::new_regs_along`] meets first, in its order,
/// so a walk past no live source and along every edge need not run: a
/// chain of joins holding a register only at its start (the first `s`)
/// was walked to that start from every join (#1502).
pub(super) struct SourceRegs {
    below: Vec<RegSet>,
    /// Where each value is defined (block, then op): a source defined
    /// before its user all the way down forms no cycle.
    pos: Vec<(u32, u32)>,
    /// Whether a value is some copy's source: only such a value, live,
    /// stops a walk.
    is_source: Vec<bool>,
    /// The registers a walk from a value meets first, in walk order
    /// (sources past params only); `None` past a cycle.
    first: Vec<Option<Vec<u8>>>,
}

impl SourceRegs {
    pub(super) fn new(func: &MirFunc, sources: &CopySources<'_>, regs: &[u8], slot: &impl Fn(ValueId) -> bool) -> Self {
        let n = func.types.len();
        let mut sets = vec![RegSet::default(); n];
        let mut users: Vec<Vec<ValueId>> = vec![Vec::new(); n];
        let mut is_source = vec![false; n];
        let mut work = Vec::new();
        for &x in sources.defs.keys() {
            for (_, s) in sources.direct(func, x) {
                if s.index() < n {
                    users[s.index()].push(x);
                    is_source[s.index()] = true;
                }
                if slot(s) {
                    sets[x.index()].insert(regs[s.index()]);
                }
            }
            work.push(x);
        }
        // Each set only grows, by at most 256 registers: a loop's φs
        // settle in a few rounds.
        while let Some(x) = work.pop() {
            let from = sets[x.index()];
            for &u in &users[x.index()] {
                let before = sets[u.index()];
                sets[u.index()].union(&from);
                if sets[u.index()] != before {
                    work.push(u);
                }
            }
        }
        let mut pos = vec![(0, 0); n];
        let mut order = Vec::new();
        for (b, block) in func.blocks.iter().enumerate() {
            for (k, inst) in block.insts.iter().enumerate() {
                let d = inst.dest();
                if d.index() < n {
                    pos[d.index()] = (b as u32 + 1, k as u32);
                    if sources.defs.contains_key(&d) {
                        order.push(d);
                    }
                }
            }
        }
        let mut params = vec![false; n];
        for p in &func.params {
            if p.index() < n {
                params[p.index()] = true;
            }
        }
        let reg = |v: ValueId| (slot(v) && !params[v.index()]).then(|| regs[v.index()]);
        // In definition order, sources first. The walk pops the last
        // source pushed first and walks all past it before the next, so
        // its first registers are its sources' own, then each source's
        // first registers from the last to the first.
        let mut first: Vec<Option<Vec<u8>>> = vec![None; n];
        for x in order {
            let srcs: Vec<ValueId> = sources.direct(func, x).map(|(_, s)| s).filter(|s| s.index() < n).collect();
            let acyclic = srcs.iter().all(|s| pos[s.index()] < pos[x.index()] && (!sources.defs.contains_key(s) || first[s.index()].is_some()));
            if !acyclic {
                continue;
            }
            let mut met = RegSet::default();
            let mut out = Vec::new();
            for &s in &srcs {
                if let Some(r) = reg(s)
                    && met.insert(r)
                {
                    out.push(r);
                }
            }
            for s in srcs.iter().rev() {
                for &r in first[s.index()].as_deref().unwrap_or(&[]) {
                    if met.insert(r) {
                        out.push(r);
                    }
                }
            }
            first[x.index()] = Some(out);
        }
        Self { below: sets, pos, is_source, first }
    }

    pub(super) fn of(&self, v: ValueId) -> &RegSet {
        static NONE: RegSet = RegSet([0; 4]);
        self.below.get(v.index()).unwrap_or(&NONE)
    }

    /// The registers [`CopySources::new_regs_along`] meets first from `v`
    /// along every edge, stopping at `live` and reporting every source
    /// with a slot that is no param, in its order: `None` when that takes
    /// the walk (a cycle, or a live source defined before `v` that may
    /// stop it).
    pub(super) fn first(&self, v: ValueId, live: &HashSet<ValueId>) -> Option<&[u8]> {
        let firsts = self.first.get(v.index())?.as_deref()?;
        let at = self.pos[v.index()];
        let stops = live.iter().any(|l| self.is_source.get(l.index()).copied().unwrap_or(false) && self.pos[l.index()] < at);
        (!stops).then_some(firsts)
    }
}

/// Each block's strongly connected component of the CFG when it lies on a
/// cycle (a loop), `None` otherwise. Kosaraju, iterative: linear in the
/// function.
fn cycle_components(func: &MirFunc) -> Vec<Option<usize>> {
    let n = func.blocks.len();
    let succs = |b: usize| -> Vec<usize> {
        func.blocks[b].term.as_ref().map(Terminator::succs).unwrap_or_default().into_iter().map(|s| s.index()).filter(|&s| s < n).collect()
    };
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    for b in 0..n {
        for s in succs(b) {
            preds[s].push(b);
        }
    }
    // Finish order of a forward DFS.
    let mut seen = vec![false; n];
    let mut order = Vec::with_capacity(n);
    for root in 0..n {
        if seen[root] {
            continue;
        }
        seen[root] = true;
        let mut stack = vec![(root, succs(root), 0usize)];
        while let Some((b, ss, i)) = stack.last_mut() {
            if let Some(&s) = ss.get(*i) {
                *i += 1;
                if !seen[s] {
                    seen[s] = true;
                    let next = succs(s);
                    stack.push((s, next, 0));
                }
            } else {
                order.push(*b);
                stack.pop();
            }
        }
    }
    // Components on the reversed graph, in reverse finish order.
    let mut comp: Vec<Option<usize>> = vec![None; n];
    let mut size: Vec<usize> = Vec::new();
    for &root in order.iter().rev() {
        if comp[root].is_some() {
            continue;
        }
        let c = size.len();
        size.push(0);
        comp[root] = Some(c);
        let mut work = vec![root];
        while let Some(b) = work.pop() {
            size[c] += 1;
            for &p in &preds[b] {
                if comp[p].is_none() {
                    comp[p] = Some(c);
                    work.push(p);
                }
            }
        }
    }
    (0..n)
        .map(|b| comp[b].filter(|&c| size[c] > 1 || succs(b).contains(&b)))
        .collect()
}

/// Registers to zero beside each `ClearWord` (by its token), whether or not
/// the cleared value's own register is zeroed: the cleared value's copy
/// sources ([`CopySources`]) that no live value shares, and that a
/// collection may still scan before they are rewritten. Without them a
/// rebound `Option` keeps its object rooted through the register the `Some`
/// was cast from (#1411). Likewise its dead copies ([`CopySinks`]: a match
/// arm's binding of the `Some`'s object) and the gather registers a dense
/// call copied it or them into ([`Gathers`]): a binding passed to a call
/// kept the object rooted in the call's argument register (#1625).
pub(super) fn source_clears(func: &MirFunc, regs: &[u8], need_slot: &[bool], gathers: &Gathers) -> HashMap<ValueId, Vec<u8>> {
    let mut out: HashMap<ValueId, Vec<u8>> = HashMap::new();
    if !func.blocks.iter().any(|b| b.insts.iter().any(|i| matches!(i, MirInst::ClearWord { .. }))) {
        return out;
    }
    let sources = CopySources::new(func);
    let sinks = CopySinks::new(func);
    let live = super::gc::liveness_after(func);
    let cmp_dests = term_cmp_dests(func);
    let comps = cycle_components(func);
    let slot = |v: ValueId| need_slot.get(v.index()).copied().unwrap_or(false) && v.index() < regs.len();
    let below = SourceRegs::new(func, &sources, regs, &slot);
    let mut scan = RewriteScan::new(func, regs, &slot, &gathers.writes);
    for (b, block) in func.blocks.iter().enumerate() {
        // A compare fused into the branch is emitted at the terminator.
        let fused_reads: Vec<ValueId> = cmp_dests[b]
            .and_then(|d| block.insts.iter().find(|i| i.dest() == d))
            .map(|i| i.operands())
            .unwrap_or_default();
        for (k, inst) in block.insts.iter().enumerate() {
            let MirInst::ClearWord { dest, value } = inst else {
                continue;
            };
            let live = &live[b][k];
            let held = |r: u8| live.iter().any(|v| slot(*v) && regs[v.index()] == r);
            // In a loop, a φ's value from outside it (left by the loop
            // entry, which `entry_clears` zeroes once) or from a back edge
            // (the previous trip's: zeroed at the join it last crossed)
            // is not cleared again on every trip; a join inside the loop
            // still is.
            let cycle = comps[b];
            let edge = |h: BlockId, p: BlockId| cycle.is_none() || (comps[p.index()] == cycle && p.index() < h.index());
            let mut rs: Vec<u8> = Vec::new();
            // Each register is asked once, however long the chain (#1493),
            // and the chain is walked only while it holds a register not
            // asked yet (#1502).
            let mut asked = RegSet::default();
            let reg = |s: ValueId| (slot(s) && !func.params.contains(&s) && !fused_reads.contains(&s)).then(|| regs[s.index()]);
            // Outside a loop, and with no heap word fused into the branch,
            // the walk's registers are known.
            let known = (cycle.is_none() && fused_reads.iter().all(|v| !func.ty(*v).is_heap_word()))
                .then(|| below.first(*value, live))
                .flatten();
            let mut met: Vec<u8> = match known {
                Some(first) => first.iter().copied().filter(|&r| asked.insert(r)).collect(),
                None => sources
                    .new_regs_along(func, *value, |x| live.contains(&x), edge, &below, reg, &mut asked)
                    .iter()
                    .map(|s| regs[s.index()])
                    .collect(),
            };
            let copies = sinks.of(*value, |x| live.contains(&x));
            for s in &copies {
                if let Some(r) = reg(*s)
                    && asked.insert(r)
                {
                    met.push(r);
                }
            }
            for x in std::iter::once(value).chain(&copies) {
                for &r in gathers.of(*x) {
                    if asked.insert(r) {
                        met.push(r);
                    }
                }
            }
            for r in met {
                if (slot(*value) && r == regs[value.index()])
                    || rs.contains(&r)
                    || held(r)
                    || copied_over_first(func, block, k, regs, r, &|v: &ValueId| slot(*v))
                    || !scan.from(block.id, k + 1, r)
                {
                    continue;
                }
                rs.push(r);
            }
            if !rs.is_empty() {
                out.insert(*dest, rs);
            }
        }
    }
    out
}

/// The heap words a value's word was copied to or holds, transitively:
/// its casts (a match arm's binding of an `Option::Some`'s object), its
/// match payloads (an arm's binding of a boxed enum's field) and, for an
/// enum or tuple it makes, the elements it was made from. The reverse of
/// [`CopySources`] through casts: a φ's copy is cleared past its edge
/// ([`entry_clears`]). Each keeps its word in its own register after its
/// last use, a call's argument or a constructor's element, so once the
/// value is rebound they still root it until rewritten (#1625).
pub(super) struct CopySinks {
    casts: HashMap<ValueId, Vec<ValueId>>,
}

impl CopySinks {
    pub(super) fn new(func: &MirFunc) -> Self {
        let mut casts: HashMap<ValueId, Vec<ValueId>> = HashMap::new();
        let payloads = payload_words(func);
        for inst in func.blocks.iter().flat_map(|b| &b.insts) {
            match inst {
                MirInst::Cast { dest, src, .. } | MirInst::MatchPayload { dest, scrutinee: src, .. }
                    if func.ty(*dest).is_heap_word() || payloads.contains(dest) =>
                {
                    casts.entry(*src).or_default().push(*dest);
                }
                MirInst::Alloc {
                    dest,
                    kind: MirAllocKind::Enum { .. } | MirAllocKind::Tuple,
                    elems,
                    ..
                } => {
                    let heap: Vec<ValueId> = elems.iter().copied().filter(|e| func.ty(*e).is_heap_word()).collect();
                    if !heap.is_empty() {
                        casts.entry(*dest).or_default().extend(heap);
                    }
                }
                _ => {}
            }
        }
        Self { casts }
    }

    /// The words of `v` not past a `stop` value (a live value: it holds
    /// the word itself). `v` itself is not listed.
    pub(super) fn of(&self, v: ValueId, stop: impl Fn(ValueId) -> bool) -> Vec<ValueId> {
        if self.casts.is_empty() {
            return Vec::new();
        }
        let mut seen: HashSet<ValueId> = HashSet::from([v]);
        let mut out = Vec::new();
        let mut work = vec![v];
        while let Some(x) = work.pop() {
            for &d in self.casts.get(&x).map(Vec::as_slice).unwrap_or(&[]) {
                if stop(d) || !seen.insert(d) {
                    continue;
                }
                out.push(d);
                work.push(d);
            }
        }
        out
    }
}

/// The `i64` words that may hold a heap word: a boxed enum's payload
/// read as a pair's low word (a two-word enum passed by value, whose
/// heap payload moves as an opaque word), the low word of a pair a call
/// returns when its enum may carry a heap payload there
/// ([`MirFunc::heap_pair_calls`]), and the φs and casts that copy one. Typed `i64`, they are no heap word to [`CopySinks`] or
/// [`arg_clears`], so a payload matched out of a rebound local, or passed
/// on to a call, kept its object rooted in their registers (#1769). A
/// plain number read the same way is zeroed too: harmless, and the
/// payload's type is not known here.
pub(super) fn payload_words(func: &MirFunc) -> HashSet<ValueId> {
    let mut out: HashSet<ValueId> = HashSet::new();
    for inst in func.blocks.iter().flat_map(|b| &b.insts) {
        if let MirInst::MatchPayload { dest, scrutinee: src, .. } | MirInst::Cast { dest, src, .. } = inst
            && func.ty(*dest) == MirTy::I64
            && func.ty(*src).is_heap_word()
        {
            out.insert(*dest);
        }
        // A pair a call returns that may carry a heap payload: its low
        // word (#1791).
        if let MirInst::Call { dest, dest_hi: Some(_), .. } = inst
            && func.ty(*dest) == MirTy::I64
            && func.heap_pair_calls.contains(dest)
        {
            out.insert(*dest);
        }
    }
    if out.is_empty() {
        return out;
    }
    // Each round adds a value or stops: a loop's φs settle in a few.
    let mut changed = true;
    while changed {
        changed = false;
        for inst in func.blocks.iter().flat_map(|b| &b.insts) {
            let copies = match inst {
                MirInst::Phi { dest, args, .. } => func.ty(*dest) == MirTy::I64 && args.iter().any(|(_, a)| out.contains(a)),
                MirInst::Cast { dest, src, .. } => func.ty(*dest) == MirTy::I64 && out.contains(src),
                _ => false,
            };
            if copies && out.insert(inst.dest()) {
                changed = true;
            }
        }
    }
    out
}

/// Per payload word ([`payload_words`]), the values it was copied from:
/// a φ's incomings, a cast's source, the enum a match read it from.
fn payload_defs(func: &MirFunc, payloads: &HashSet<ValueId>) -> HashMap<ValueId, Vec<ValueId>> {
    func.blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter(|i| payloads.contains(&i.dest()))
        .filter_map(|i| match i {
            MirInst::Phi { args, .. } => Some((i.dest(), args.iter().map(|&(_, a)| a).collect())),
            MirInst::Cast { src, .. } | MirInst::MatchPayload { scrutinee: src, .. } => Some((i.dest(), vec![*src])),
            _ => None,
        })
        .collect()
}

/// The values a payload word was copied from ([`payload_defs`]) that
/// still hold its object, transitively, not past a `stop` value. `v`
/// itself is not listed.
fn payload_sources(
    func: &MirFunc,
    payloads: &HashSet<ValueId>,
    defs: &HashMap<ValueId, Vec<ValueId>>,
    v: ValueId,
    stop: impl Fn(ValueId) -> bool,
) -> Vec<ValueId> {
    let mut seen: HashSet<ValueId> = HashSet::from([v]);
    let mut out = Vec::new();
    let mut work = vec![v];
    while let Some(x) = work.pop() {
        for &s in defs.get(&x).map(Vec::as_slice).unwrap_or(&[]) {
            if stop(s) || !seen.insert(s) || !(func.ty(s).is_heap_word() || payloads.contains(&s)) {
                continue;
            }
            out.push(s);
            work.push(s);
        }
    }
    out
}

/// The gather registers a dense body's pushes copy heap words into
/// ([`gather_base`]: arguments not in consecutive registers are moved to
/// the scratch window first). Nothing else writes those registers, so
/// each keeps the last word copied there after the call: the argument's
/// object stays rooted however its value is cleared (#1625). The LIR tier
/// pushes from the value's own register and gathers nothing.
#[derive(Default)]
pub(super) struct Gathers {
    /// Per argument value, the scratch registers it may be copied into.
    by_value: HashMap<ValueId, Vec<u8>>,
    /// The scratch registers each push writes, by block index and op
    /// index: before the op runs.
    pub(super) writes: Vec<(usize, usize, u8)>,
    /// Per push (block index, op index), the scratch registers it gathers
    /// words into and the word each takes.
    by_push: HashMap<(usize, usize), Vec<(u8, ValueId)>>,
}

impl Gathers {
    pub(super) fn new(func: &MirFunc, regs: &[u8], need_slot: &[bool], scratch: u8, self_entry: Option<Label>) -> Self {
        let mut out = Self::default();
        let slot = |v: ValueId| need_slot.get(v.index()).copied().unwrap_or(false) && v.index() < regs.len();
        // A payload word moving as an `i64` holds its object too (#1791).
        let payloads = payload_words(func);
        for (b, block) in func.blocks.iter().enumerate() {
            for (k, inst) in block.insts.iter().enumerate() {
                if is_tail_call_inst(func, block, inst) {
                    continue;
                }
                // The words each push takes, in the order `emit_inst` pushes
                // them (an enum's both ways: `DenseMake` reads them reversed).
                let orders: Vec<Vec<ValueId>> = match inst {
                    MirInst::Call { args, target, .. } if Some(*target) != self_entry => vec![args.clone()],
                    MirInst::HostInvoke { args, .. } => vec![args.clone()],
                    MirInst::CallIndirect { args, callee, .. } => vec![args.iter().copied().chain([*callee]).collect()],
                    MirInst::Alloc { kind: MirAllocKind::Enum { .. }, elems, .. } => {
                        vec![elems.clone(), elems.iter().rev().copied().collect()]
                    }
                    MirInst::Alloc { elems, .. } => vec![elems.clone()],
                    MirInst::Format { fmt, args, .. } => vec![std::iter::once(*fmt).chain(args.iter().copied()).collect()],
                    MirInst::StrEq { lhs, rhs, .. } => vec![vec![*lhs, *rhs]],
                    _ => continue,
                };
                for words in orders {
                    if !words.iter().all(|w| slot(*w)) {
                        continue;
                    }
                    let slots: Vec<u8> = words.iter().map(|w| regs[w.index()]).collect();
                    if slots.windows(2).all(|w| w[1] == w[0].saturating_add(1)) {
                        continue;
                    }
                    for (i, w) in words.iter().enumerate() {
                        let Some(r) = scratch.checked_add(i as u8) else { break };
                        if r == slots[i] || !(func.ty(*w).is_heap_word() || payloads.contains(w)) {
                            continue;
                        }
                        let rs = out.by_value.entry(*w).or_default();
                        if !rs.contains(&r) {
                            rs.push(r);
                        }
                        if !out.writes.contains(&(b, k, r)) {
                            out.writes.push((b, k, r));
                        }
                        let ws = out.by_push.entry((b, k)).or_default();
                        if !ws.contains(&(r, *w)) {
                            ws.push((r, *w));
                        }
                    }
                }
            }
        }
        out
    }

    fn of(&self, v: ValueId) -> &[u8] {
        self.by_value.get(&v).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// Registers to zero right after a call, or an op that stores a word
/// into an object ([`consumes`]), by its dest: those of a heap word it
/// takes whose last use it is (a temporary passed straight to it,
/// `weak(new Node(..))`, `v.push(new Node(..))`, `Some(new Node(..))`),
/// and the gather registers a dense op copied such a word into
/// ([`Gathers`]). No `ClearWord` names a temporary, so without them the
/// op's operand register kept the object rooted until rewritten (#1650,
/// #1690). Only where a collection may
/// come before the register is rewritten ([`RewriteScan`]), and never for
/// a local's word ([`MirFunc::bound_words`]: the frame holds it until the
/// local is rebound, as a `gc::Root` guard relies on), a parameter (the
/// caller roots it), a constant, or a register a value live past the call
/// shares. `emitted_at` names the op each op's zeros follow: itself, or
/// for an op the LIR tier emits inside an expression tree, the tree's
/// root (`v.push(Holder::One(new Node(..)))`: the enum's alloc is an
/// inline operand of the push), when no op up to the root writes the
/// register; `None` when it has none.
///
/// A call ending its block whose result the branch takes from the stack
/// (`write(..)?`'s `JumpIfMatch`) keeps it there: its zeros go at the top
/// of each successor instead ([`ArgClears::entry`]), when this block is
/// that successor's only predecessor. The branch cannot collect.
pub(super) fn arg_clears(
    func: &MirFunc,
    regs: &[u8],
    need_slot: &[bool],
    gathers: &Gathers,
    emitted_at: impl Fn(ValueId) -> Option<ValueId>,
) -> ArgClears {
    let mut out = ArgClears::default();
    let slot = |v: ValueId| need_slot.get(v.index()).copied().unwrap_or(false) && v.index() < regs.len();
    let payloads = payload_words(func);
    let heapish = |v: ValueId| func.ty(v).is_heap_word() || payloads.contains(&v);
    // A heap word cast from another (`Option::Some(new Node(..))`'s niche
    // word): the LIR tier emits it as an inline operand with no register
    // of its own, so its source's register is what holds the object
    // (#1802). Walked like a payload word.
    let mut copied = payloads.clone();
    for inst in func.blocks.iter().flat_map(|b| &b.insts) {
        if let MirInst::Cast { dest, src, .. } = inst
            && func.ty(*dest).is_heap_word()
            && heapish(*src)
        {
            copied.insert(*dest);
        }
    }
    let payload_defs = payload_defs(func, &copied);
    let mut bound_regs = RegSet::default();
    for &v in &func.bound_words {
        if slot(v) {
            bound_regs.insert(regs[v.index()]);
        }
    }
    let heap_temp = |v: ValueId| slot(v) && heapish(v) && !func.params.contains(&v);
    let candidate = |v: ValueId| heap_temp(v) && !func.bound_words.contains(&v);
    // A push's dest is its array, which the local is rebound to: the
    // array's old word may go even when a local bound it.
    let operand_candidate = |inst: &MirInst, v: ValueId| match inst {
        MirInst::ArrayPush { array, .. } if *array == v => heap_temp(v),
        _ => candidate(v),
    };
    let dead_stores = unused_store_dests(func);
    let any = func.blocks.iter().any(|b| {
        b.insts.iter().any(|i| {
            consumes(i)
                && !is_tail_call_inst(func, b, i)
                && (i.operands().into_iter().any(|v| operand_candidate(i, v)) || dead_stores.contains(&i.dest()))
        })
    });
    if !any {
        return out;
    }
    // A constant (a `None` niche, a pooled string), or a cast of one,
    // roots nothing. A box of plain words (an `Option<int>`, a pair of
    // numbers, made to pass a value to `weak` or a generic parameter)
    // holds no other object, and nothing but the call's register can hold
    // that box: it stays as it was, so a weak to it still upgrades until
    // the register is rewritten.
    let mut consts: HashSet<ValueId> = HashSet::new();
    for inst in func.blocks.iter().flat_map(|b| &b.insts) {
        match inst {
            MirInst::Const { dest, .. } => {
                consts.insert(*dest);
            }
            MirInst::Alloc {
                dest,
                kind: MirAllocKind::Enum { .. } | MirAllocKind::Tuple | MirAllocKind::Boxed { .. },
                elems,
                ..
            } if elems.iter().all(|e| !heapish(*e)) => {
                consts.insert(*dest);
            }
            MirInst::Cast { dest, src, .. } if consts.contains(src) => {
                consts.insert(*dest);
            }
            MirInst::GcBarrier { dest, roots, .. } if roots.len() == 1 && consts.contains(&roots[0]) && func.ty(*dest).is_heap_word() => {
                consts.insert(*dest);
            }
            _ => {}
        }
    }
    let live = super::gc::liveness_after(func);
    let cmp_dests = term_cmp_dests(func);
    let preds = func.preds();
    let mut scan = RewriteScan::new(func, regs, &slot, &gathers.writes);
    for (b, block) in func.blocks.iter().enumerate() {
        // A compare fused into the branch is emitted at the terminator.
        let fused_reads: Vec<ValueId> = cmp_dests[b]
            .and_then(|d| block.insts.iter().find(|i| i.dest() == d))
            .map(|i| i.operands())
            .unwrap_or_default();
        let index: HashMap<ValueId, usize> = block.insts.iter().enumerate().map(|(k, i)| (i.dest(), k)).collect();
        for (k, inst) in block.insts.iter().enumerate() {
            if !consumes(inst) || is_tail_call_inst(func, block, inst) {
                continue;
            }
            // The op the zeros follow: this one, or the root of the
            // expression tree the LIR tier emits it inside.
            let Some(at) = emitted_at(inst.dest()) else {
                continue;
            };
            let Some(&k_at) = index.get(&at) else {
                continue;
            };
            let root = &block.insts[k_at];
            if k_at < k || is_tail_call_inst(func, block, root) || cmp_dests[b] == Some(at) {
                continue;
            }
            let unused = |v: ValueId| !consts.contains(&v) && !live[b][k].contains(&v) && !fused_reads.contains(&v);
            let mut words: Vec<(u8, ValueId)> = inst
                .operands()
                .into_iter()
                .filter(|&v| operand_candidate(inst, v) && unused(v))
                .map(|v| (regs[v.index()], v))
                .collect();
            // A payload word passed on: the registers it was copied from
            // (its φ's incomings, the match it was read by and the enum
            // matched, the object cast to it) hold the object too, also
            // when the word itself has none (an inline operand of the LIR
            // tier) (#1769). Likewise a heap word cast from another, a
            // niche `Option::Some` (#1802). Not past a live value, nor
            // into a register a local's word shares.
            for v in inst.operands() {
                if !copied.contains(&v) || func.params.contains(&v) || func.bound_words.contains(&v) || !unused(v) {
                    continue;
                }
                for s in payload_sources(func, &payloads, &payload_defs, v, |x| consts.contains(&x) || live[b][k].contains(&x)) {
                    if candidate(s) && unused(s) && !bound_regs.contains(regs[s.index()]) {
                        words.push((regs[s.index()], s));
                    }
                }
            }
            for &(r, v) in gathers.by_push.get(&(b, k)).map(Vec::as_slice).unwrap_or(&[]) {
                if candidate(v) && unused(v) {
                    words.push((r, v));
                }
            }
            // A store's dest nothing reads: the copy of the stored word the
            // dense tier moves into its register (the LIR tier gives it none).
            for d in inst.dests() {
                if dead_stores.contains(&d) && candidate(d) && !consts.contains(&d) {
                    words.push((regs[d.index()], d));
                }
            }
            // A tree's ops up to its root are emitted before the zeros:
            // none may write the register, or take a local's word in it.
            let span = &block.insts[k..=k_at];
            let mut zero: Vec<u8> = Vec::new();
            for (r, _) in words {
                if zero.contains(&r)
                    || inst.dests().any(|d| slot(d) && !dead_stores.contains(&d) && regs[d.index()] == r)
                    || span[1..].iter().flat_map(MirInst::dests).any(|d| slot(d) && regs[d.index()] == r)
                    // A store's dest may share its value's register: a
                    // local's word stays.
                    || span.iter().any(|i| {
                        i.operands()
                            .into_iter()
                            .any(|o| slot(o) && regs[o.index()] == r && !(operand_candidate(i, o) && !consts.contains(&o)))
                    })
                    || live[b][k_at].iter().any(|&x| slot(x) && regs[x.index()] == r)
                    || !scan.from(block.id, k + 1, r)
                {
                    continue;
                }
                zero.push(r);
            }
            if zero.is_empty() {
                continue;
            }
            if let Some(succs) = branch_takes(block, k_at, at) {
                let only_pred = |s: BlockId| preds[s.index()].len() == 1 && !func.block(s).insts.first().is_some_and(MirInst::is_phi);
                if succs.iter().all(|&s| only_pred(s)) {
                    for s in succs {
                        let rs: Vec<u8> = zero.iter().copied().filter(|&r| scan.from(s, 0, r)).collect();
                        if !rs.is_empty() {
                            out.entry.entry(s).or_default().extend(rs);
                        }
                    }
                    continue;
                }
            }
            let after = out.after.entry(at).or_default();
            for r in zero {
                if !after.contains(&r) {
                    after.push(r);
                }
            }
        }
    }
    out
}

/// The ops [`arg_clears`] zeroes the dead operands of: a call (not a
/// tail call), and an op that stores a word into an object, so the
/// object holds it from there and its register only kept a copy: an
/// array push, an allocation (an enum, tuple, record or closure built
/// from its elems), a field or element store, a static store (#1690).
/// An op that only reads a temporary (a field load of `f().x`) is left
/// as it was.
fn consumes(inst: &MirInst) -> bool {
    matches!(
        inst,
        MirInst::Call { .. }
            | MirInst::HostInvoke { .. }
            | MirInst::CallIndirect { .. }
            | MirInst::ArrayPush { .. }
            | MirInst::Alloc { .. }
            | MirInst::HeapFieldStore { .. }
            | MirInst::StoreIndex { .. }
            | MirInst::StoreStatic { .. }
    )
}

/// The dests of field and element stores nothing reads. A store's dest
/// is the word it stored (the VM leaves it on the stack), so writing it
/// to a register kept a copy of the stored object rooted there until the
/// register was rewritten (`new Pair(new Node(..))`, #1690): emission
/// zeroes it after the store ([`arg_clears`], the dense tier, which moves
/// the word into the dest's register), or writes none (the LIR tier pops
/// the word).
pub(super) fn unused_store_dests(func: &MirFunc) -> HashSet<ValueId> {
    let mut stores: HashSet<ValueId> = func
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter_map(|i| match i {
            MirInst::HeapFieldStore { dest, .. } | MirInst::StoreIndex { dest, .. } => Some(*dest),
            _ => None,
        })
        .collect();
    if stores.is_empty() {
        return stores;
    }
    for block in &func.blocks {
        for inst in &block.insts {
            for v in inst.operands() {
                stores.remove(&v);
            }
        }
        if let Some(term) = &block.term {
            for v in super::gc::term_uses(term) {
                stores.remove(&v);
            }
        }
    }
    for v in func.debug_slots.values() {
        stores.remove(v);
    }
    stores
}

/// The zeros [`arg_clears`] places: after a call (by its dest), or at the
/// top of a block, below its label.
#[derive(Default)]
pub(super) struct ArgClears {
    pub(super) after: HashMap<ValueId, Vec<u8>>,
    pub(super) entry: HashMap<BlockId, Vec<u8>>,
}

impl ArgClears {
    pub(super) fn after(&self, dest: ValueId) -> &[u8] {
        self.after.get(&dest).map(Vec::as_slice).unwrap_or(&[])
    }

    pub(super) fn entry(&self, block: BlockId) -> &[u8] {
        self.entry.get(&block).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// The successors of `block` when its op `k` is a call whose result
/// `dest` its branch reads, and nothing is emitted between them (only the
/// branch's own payloads follow, which it binds).
fn branch_takes(block: &super::func::MirBlock, k: usize, dest: ValueId) -> Option<Vec<BlockId>> {
    let term = block.term.as_ref()?;
    let bound = |v: &ValueId| matches!(term, Terminator::JumpIfMatch { payloads, .. } if payloads.contains(v));
    if !block.insts[k + 1..]
        .iter()
        .all(|i| matches!(i, MirInst::MatchPayload { dest: d, scrutinee, .. } if *scrutinee == dest && bound(d)))
    {
        return None;
    }
    match term {
        Terminator::JumpIfMatch { scrutinee, taken, not_taken, .. } if *scrutinee == dest => Some(vec![*taken, *not_taken]),
        Terminator::Br { cond, taken, not_taken } if *cond == dest => Some(vec![*taken, *not_taken]),
        _ => None,
    }
}

/// Drops the GC clears that zero a register no path into them leaves
/// holding a word this frame wrote since its entry: one an earlier clear
/// on every path zeroed with no write after, or one no path wrote (#1519).
/// Neither holds a root a clear could drop: the precise frame maps count a
/// word this frame never wrote as no root, as they do a zero. A chain of
/// joins re-zeroed, at each taken arm, every register an earlier value of
/// the chain lived in, twice (beside its `ClearWord`, and again past the
/// edge's φ copies).
///
/// A forward dataflow over the blocks of the registers that may hold such
/// a word. Writes count where emission may put them: every register a
/// block's ops define is taken as written at its top and again before its
/// terminator, besides where the op stands; an edge writes its φs and its
/// target's match payloads before its clears; a word with a register that
/// no op defines (a terminator's payload) is never taken as zeroed.
pub(super) fn prune_clears(
    func: &MirFunc,
    regs: &[u8],
    need_slot: &[bool],
    cleared: &mut HashSet<ValueId>,
    entry: &mut HashMap<(BlockId, BlockId), Vec<u8>>,
    source: &mut HashMap<ValueId, Vec<u8>>,
    gather_writes: &[(usize, usize, u8)],
) {
    if cleared.is_empty() && entry.is_empty() && source.is_empty() {
        return;
    }
    let slot = |v: ValueId| need_slot.get(v.index()).copied().unwrap_or(false) && v.index() < regs.len();
    let nb = func.blocks.len();
    // Registers each block's ops (not its φs) define, and those of its φs
    // and match payloads an edge into it writes.
    let mut defs = vec![RegSet::default(); nb];
    let mut edge_writes = vec![RegSet::default(); nb];
    let mut defined = vec![false; regs.len()];
    for (b, block) in func.blocks.iter().enumerate() {
        for inst in &block.insts {
            for d in inst.dests() {
                if d.index() < defined.len() {
                    defined[d.index()] = true;
                }
                if !slot(d) {
                    continue;
                }
                if inst.is_phi() || matches!(inst, MirInst::MatchPayload { .. }) {
                    edge_writes[b].insert(regs[d.index()]);
                }
                if !inst.is_phi() {
                    defs[b].insert(regs[d.index()]);
                }
            }
        }
    }
    // A push's gather registers ([`Gathers`]) are written where it stands.
    for &(b, _, r) in gather_writes {
        if b < nb {
            defs[b].insert(r);
        }
    }
    // Parameters arrive holding words; a word no op defines may be written
    // anywhere.
    let mut wild = RegSet::default();
    for p in &func.params {
        if slot(*p) {
            wild.insert(regs[p.index()]);
        }
    }
    for (i, d) in defined.iter().enumerate() {
        if !d && slot(ValueId(i as u32)) {
            wild.insert(regs[i]);
        }
    }
    // Runs `block` from `dirty`; `keep` hears each clear's register and
    // whether it may hold a word there. Returns what each edge leaves.
    let run = |b: usize, mut dirty: RegSet, keep: &mut dyn FnMut(Clear, u8, bool)| -> Vec<(BlockId, RegSet)> {
        let block = &func.blocks[b];
        dirty.union(&defs[b]);
        dirty.union(&wild);
        for inst in &block.insts {
            if inst.is_phi() {
                continue;
            }
            if let MirInst::ClearWord { dest, value } = inst {
                if cleared.contains(dest) && slot(*value) {
                    let r = regs[value.index()];
                    keep(Clear::Own(*dest), r, dirty.contains(r));
                    dirty.remove(r);
                }
                for &r in source.get(dest).map(Vec::as_slice).unwrap_or(&[]) {
                    keep(Clear::Source(*dest), r, dirty.contains(r));
                    dirty.remove(r);
                }
                dirty.union(&wild);
                continue;
            }
            for d in inst.dests() {
                if slot(d) {
                    dirty.insert(regs[d.index()]);
                }
            }
        }
        dirty.union(&defs[b]);
        let Some(term) = &block.term else {
            return Vec::new();
        };
        let succs = term.succs();
        succs
            .iter()
            .map(|&s| {
                let mut out = dirty;
                if s.index() < nb {
                    out.union(&edge_writes[s.index()]);
                }
                for &r in entry.get(&(block.id, s)).map(Vec::as_slice).unwrap_or(&[]) {
                    keep(Clear::Edge(block.id, s), r, out.contains(r));
                    out.remove(r);
                }
                out.union(&wild);
                (s, out)
            })
            .collect()
    };
    // Fixpoint: the registers that may hold a word at each block's top.
    let mut at: Vec<Option<RegSet>> = vec![None; nb];
    if func.entry.index() >= nb {
        return;
    }
    at[func.entry.index()] = Some(wild);
    let mut work = vec![func.entry.index()];
    let mut queued = vec![false; nb];
    queued[func.entry.index()] = true;
    while let Some(b) = work.pop() {
        queued[b] = false;
        let Some(dirty) = at[b] else { continue };
        for (s, out) in run(b, dirty, &mut |_, _, _| {}) {
            let s = s.index();
            if s >= nb {
                continue;
            }
            let next = match at[s] {
                Some(mut cur) => {
                    if cur.contains_all(&out) {
                        continue;
                    }
                    cur.union(&out);
                    cur
                }
                None => out,
            };
            at[s] = Some(next);
            if !queued[s] {
                queued[s] = true;
                work.push(s);
            }
        }
    }
    // A clear is dropped where its register holds no such word; a block
    // nothing reaches keeps its clears.
    let mut drop_own: HashSet<ValueId> = HashSet::new();
    let mut keep_source: HashMap<ValueId, Vec<u8>> = HashMap::new();
    let mut keep_edge: HashMap<(BlockId, BlockId), Vec<u8>> = HashMap::new();
    for (b, dirty) in at.iter().enumerate() {
        let Some(dirty) = *dirty else { continue };
        run(b, dirty, &mut |c, r, held| match c {
            Clear::Own(t) => {
                if !held {
                    drop_own.insert(t);
                }
            }
            Clear::Source(t) => {
                let rs = keep_source.entry(t).or_default();
                if held {
                    rs.push(r);
                }
            }
            Clear::Edge(p, s) => {
                let rs = keep_edge.entry((p, s)).or_default();
                if held {
                    rs.push(r);
                }
            }
        });
    }
    cleared.retain(|t| !drop_own.contains(t));
    for (t, rs) in keep_source {
        if rs.is_empty() {
            source.remove(&t);
        } else {
            source.insert(t, rs);
        }
    }
    for (e, rs) in keep_edge {
        if rs.is_empty() {
            entry.remove(&e);
        } else {
            entry.insert(e, rs);
        }
    }
}

/// A clear [`prune_clears`] weighs: a `ClearWord`'s own register, one of
/// its copy sources', or one past an edge's φ copies.
#[derive(Clone, Copy)]
enum Clear {
    Own(ValueId),
    Source(ValueId),
    Edge(BlockId, BlockId),
}

/// `CONST 0; STORE reg`, as HIR emission's clearing store: opaque, so
/// dead-store elimination keeps it (the register is never read again,
/// but the collector scans it).
pub(super) fn push_clear_word(out: &mut Vec<IlOp>, reg: u8) {
    out.push(IlOp::Const { imm: 0, loc: DebugLoc::unknown() });
    out.push(IlOp::Byte {
        byte: Byte::new(Instruction::STORE).with_load_store_slot(u32::from(reg)),
        loc: DebugLoc::unknown(),
    });
}

pub(super) fn assign_regs(func: &MirFunc, need_slot: &[bool]) -> Result<(Vec<u8>, u8), LowerError> {
    assign_regs_prefer_payloads(func, need_slot)
}

pub(super) fn assign_regs_prefer_payloads(
    func: &MirFunc,
    need_slot: &[bool],
) -> Result<(Vec<u8>, u8), LowerError> {
    let n = func.types.len();
    let mut reg = vec![0u8; n];
    let mut taken = vec![false; n];
    for (i, &p) in func.params.iter().enumerate() {
        if i > 254 {
            return Err(LowerError::Refused("too many params".into()));
        }
        reg[p.index()] = i as u8;
        taken[p.index()] = true;
    }
    let mut next = func.params.len();
    next = assign_match_payload_regs(func, need_slot, &mut reg, &mut taken, next)?;
    for i in 0..n {
        if !need_slot.get(i).copied().unwrap_or(false) || taken[i] {
            continue;
        }
        if next > 254 {
            return Err(LowerError::Refused("too many dense slots".into()));
        }
        reg[i] = next as u8;
        taken[i] = true;
        next += 1;
    }
    Ok((reg, next as u8))
}

fn assign_match_payload_regs(
    func: &MirFunc,
    need_slot: &[bool],
    reg: &mut [u8],
    taken: &mut [bool],
    mut next: usize,
) -> Result<usize, LowerError> {
    let mut dests: Vec<(u32, ValueId)> = Vec::new();
    for b in &func.blocks {
        for inst in &b.insts {
            if let MirInst::MatchPayload { dest, index, .. } = inst {
                if !need_slot.get(dest.index()).copied().unwrap_or(false) {
                    continue;
                }
                if taken[dest.index()] {
                    continue;
                }
                dests.push((*index, *dest));
            }
        }
    }
    dests.sort_by_key(|(index, dest)| (*index, dest.0));
    dests.dedup_by_key(|(_, dest)| dest.0);
    for (_, dest) in dests {
        if next > 254 {
            return Err(LowerError::Refused("too many match payload slots".into()));
        }
        reg[dest.index()] = next as u8;
        taken[dest.index()] = true;
        next += 1;
    }
    Ok(next)
}

pub(super) fn next_emitted(func: &MirFunc, from: BlockId) -> Option<BlockId> {
    if from == func.entry {
        return func.blocks.iter().find(|b| b.id != func.entry).map(|b| b.id);
    }
    func.blocks
        .iter()
        .skip_while(|b| b.id != from)
        .skip(1)
        .find(|b| b.id != func.entry)
        .map(|b| b.id)
}

pub(super) fn is_fallthrough(func: &MirFunc, from: BlockId, to: BlockId) -> bool {
    next_emitted(func, from) == Some(to)
}

/// Whether `a` is emitted no later than `b` (a jump from `b` to `a` goes back).
fn emitted_before(func: &MirFunc, a: BlockId, b: BlockId) -> bool {
    func.blocks.iter().find(|x| x.id == a || x.id == b).is_some_and(|x| x.id == a)
}

/// Blocks whose whole body is the compare their `Br` branches on (a
/// `while i < n` header): a back edge can repeat that compare-branch instead
/// of jumping to it (#1867). Nothing else may be emitted at the header's
/// start or on its out-edges (φ copies, clears, a match chain's stack), so
/// the copy at the latch is the header's code exactly.
fn rotatable_headers(
    func: &MirFunc,
    cmp_dests: &[Option<ValueId>],
    preds: &[Vec<BlockId>],
    arg_clears: &ArgClears,
    entry_clears: &HashMap<(BlockId, BlockId), Vec<u8>>,
    regs: &[u8],
    scratch: u8,
) -> Vec<bool> {
    let mut out = vec![false; func.blocks.len()];
    for b in &func.blocks {
        if b.id == func.entry || !arg_clears.entry(b.id).is_empty() {
            continue;
        }
        let Some(Terminator::Br { taken, not_taken, .. }) = &b.term else {
            continue;
        };
        let Some(cmp) = cmp_dests[b.id.index()] else {
            continue;
        };
        let body_ok = b.insts.iter().all(|i| {
            (i.is_phi() || i.dest() == cmp) && arg_clears.after(i.dest()).is_empty()
        });
        let edges_ok = [*taken, *not_taken].into_iter().all(|s| {
            phi_moves(func, b.id, s, regs, scratch).is_empty()
                && entry_clears.get(&(b.id, s)).is_none_or(Vec::is_empty)
        });
        let preds_ok = preds[b.id.index()]
            .iter()
            .all(|&p| matches!(func.block(p).term, Some(Terminator::Jump { .. } | Terminator::Br { .. })));
        out[b.id.index()] = body_ok && edges_ok && preds_ok && taken != not_taken;
    }
    out
}

/// Alias a header φ dest with its latch incoming when the dest is dead after
/// that incoming is defined (so `i = i + 1` is a dest-overwrite, not a move).
/// COI-383 S6 sinks the incoming first so mandelbrot `tr`/`zr` can alias.
/// Refuses when another φ in the header reads this dest from the same edge
/// (`prev' = curr` beside `curr' = prev + curr`).
pub(super) fn coalesce_latch_overwrite(
    func: &MirFunc,
    mut regs: Vec<u8>,
    need_slot: &[bool],
) -> Vec<u8> {
    let _ = need_slot;
    for block in &func.blocks {
        for inst in &block.insts {
            let MirInst::Phi { dest, args, .. } = inst else {
                continue;
            };
            let Some((pred, latch_val)) = args
                .iter()
                .find(|(pred, _)| pred.index() > block.id.index())
            else {
                continue;
            };
            if !latch_overwrite_ok(func, block.id, *pred, *dest, *latch_val) {
                continue;
            }
            if dest.index() < regs.len() && latch_val.index() < regs.len() {
                regs[dest.index()] = regs[latch_val.index()];
            }
        }
    }
    let regs = coalesce_join_phis(func, regs, need_slot);
    coalesce_join_incomings(func, regs, need_slot)
}

/// Give an element or field store's dest that nothing reads its value's
/// register. `DenseStoreIndex` / `DenseFieldStore` read the stored word
/// from the dest byte and write no register, so a dest of its own cost a
/// `DenseMove` before every store (`flags[k] = 0` in a loop, #1867).
fn alias_dead_store_dests(func: &MirFunc, mut regs: Vec<u8>) -> Vec<u8> {
    let dead = unused_store_dests(func);
    if dead.is_empty() {
        return regs;
    }
    for inst in func.blocks.iter().flat_map(|b| &b.insts) {
        let (MirInst::StoreIndex { dest, value, .. } | MirInst::HeapFieldStore { dest, value, .. }) = inst else {
            continue;
        };
        if dead.contains(dest) && dest.index() < regs.len() && value.index() < regs.len() {
            regs[dest.index()] = regs[value.index()];
        }
    }
    regs
}

/// Alias a join φ dest with the φ of the next join that is its only use,
/// when its block jumps straight there (`else if` chains): the inner join's
/// incoming copies then land in the outer dest, and the copy between the
/// joins disappears. Registers are one per value, so the outer dest holds
/// nothing else on the paths into the inner join.
fn coalesce_join_phis(func: &MirFunc, mut regs: Vec<u8>, need_slot: &[bool]) -> Vec<u8> {
    let mut uses: HashMap<ValueId, usize> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            for v in inst.operands() {
                *uses.entry(v).or_default() += 1;
            }
        }
        if let Some(term) = &block.term {
            let mut t = term.clone();
            t.rewrite_values(|v| {
                *uses.entry(v).or_default() += 1;
                v
            });
        }
    }
    let n = regs.len();
    let slot = |v: ValueId| need_slot.get(v.index()).copied().unwrap_or(false) && v.index() < n;
    let phis = |id: BlockId| {
        func.block(id).insts.iter().filter_map(|inst| match inst {
            MirInst::Phi { dest, args, .. } => Some((*dest, args)),
            _ => None,
        })
    };
    // Outer joins first, so a chain takes the outermost register.
    for block in func.blocks.iter().rev() {
        for (dest, args) in phis(block.id) {
            for &(pred, v) in args.iter() {
                let inner = func.block(pred);
                let only_jump = matches!(inner.term, Some(Terminator::Jump { dest: to }) if to == block.id);
                let forward = |b: &super::func::MirBlock| phis(b.id).all(|(_, a)| a.iter().all(|(p, _)| p.index() < b.id.index()));
                if pred.index() < block.id.index()
                    && only_jump
                    && forward(inner)
                    && phis(pred).any(|(d, _)| d == v)
                    && uses.get(&v) == Some(&1)
                    // A block branching to both joins would copy into the
                    // shared register on each edge.
                    && !phis(pred).any(|(_, a)| a.iter().any(|(p, _)| args.iter().any(|(q, _)| q == p)))
                    && slot(v)
                    && slot(dest)
                {
                    regs[v.index()] = regs[dest.index()];
                }
            }
        }
    }
    regs
}

/// Give a forward join's scalar φ incoming the φ's register when the
/// incoming is computed in the predecessor that jumps straight to the join
/// and the φ is its only use: `return Ok(x + 1)` in one arm of an
/// inlined callee then computes into the join's register, and the edge copy
/// disappears. Scalars only: heap words keep their own registers for the
/// edge `ClearWord`s. The incoming's live range is that block's tail, where
/// nothing else in the φ's register is live (the φ is defined at the join,
/// which does not dominate a forward predecessor).
fn coalesce_join_incomings(func: &MirFunc, mut regs: Vec<u8>, need_slot: &[bool]) -> Vec<u8> {
    let n = regs.len();
    let slot = |v: ValueId| v.index() < n && need_slot.get(v.index()).copied().unwrap_or(false);
    let scalar = |v: ValueId| matches!(func.types.get(v.index()), Some(MirTy::I64 | MirTy::F64 | MirTy::Bool));
    let mut uses: HashMap<ValueId, usize> = HashMap::new();
    let mut def_in: HashMap<ValueId, BlockId> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            for v in inst.operands() {
                *uses.entry(v).or_default() += 1;
            }
            if matches!(
                inst,
                MirInst::Const { .. } | MirInst::Bin { .. } | MirInst::Cmp { .. } | MirInst::Unary { .. } | MirInst::Cast { .. }
            ) {
                def_in.insert(inst.dest(), block.id);
            }
        }
        if let Some(term) = &block.term {
            let mut t = term.clone();
            t.rewrite_values(|v| {
                *uses.entry(v).or_default() += 1;
                v
            });
        }
    }
    let mut sharing: HashMap<u8, usize> = HashMap::new();
    for (i, &r) in regs.iter().enumerate() {
        if need_slot.get(i).copied().unwrap_or(false) {
            *sharing.entry(r).or_default() += 1;
        }
    }
    for block in &func.blocks {
        let phis: Vec<(ValueId, &Vec<(BlockId, ValueId)>)> = block
            .insts
            .iter()
            .map_while(|inst| match inst {
                MirInst::Phi { dest, args, .. } => Some((*dest, args)),
                _ => None,
            })
            .collect();
        // A loop header's φ is live around the back edge.
        if phis.iter().any(|(_, args)| args.iter().any(|(p, _)| p.index() >= block.id.index())) {
            continue;
        }
        for (dest, args) in phis {
            if !slot(dest) || !scalar(dest) {
                continue;
            }
            for &(pred, v) in args {
                let only_jump = matches!(func.block(pred).term, Some(Terminator::Jump { dest: to }) if to == block.id);
                if only_jump
                    && slot(v)
                    && func.types.get(v.index()) == func.types.get(dest.index())
                    && def_in.get(&v) == Some(&pred)
                    && uses.get(&v) == Some(&1)
                    && sharing.get(&regs[v.index()]) == Some(&1)
                {
                    *sharing.entry(regs[v.index()]).or_default() -= 1;
                    regs[v.index()] = regs[dest.index()];
                    *sharing.entry(regs[dest.index()]).or_default() += 1;
                }
            }
        }
    }
    regs
}

fn latch_overwrite_ok(
    func: &MirFunc,
    header: BlockId,
    latch: BlockId,
    dest: ValueId,
    latch_val: ValueId,
) -> bool {
    // Another φ in this header takes `dest` from the same predecessor
    // (`prev' = curr` while `curr' = prev + curr`). The copy is emitted at
    // the back edge, after the add. Aliasing `curr` with the sum clobbers
    // `curr` first, so `prev` sees the new value and the pair doubles.
    for inst in &func.block(header).insts {
        let MirInst::Phi {
            dest: other, args, ..
        } = inst
        else {
            break;
        };
        if *other == dest {
            continue;
        }
        if args.iter().any(|(pred, src)| *pred == latch && *src == dest) {
            return false;
        }
    }
    let block = func.block(latch);
    let mut seen_def = false;
    for inst in &block.insts {
        if inst.dest() == latch_val {
            seen_def = true;
            continue;
        }
        // A rebind's `ClearWord` of the old value reads no word: once
        // aliased, `cleared_words` sees the live latch value in the shared
        // register and skips the zero.
        if seen_def && !matches!(inst, MirInst::ClearWord { .. }) && inst.operands().contains(&dest) {
            return false;
        }
    }
    if !seen_def {
        // CSE can merge `i+1` with an earlier body use (SROA last-arm
        // `xs[1]`). Aliasing the header φ with that value then clobbers
        // `i` before later arms / `i % n`.
        return false;
    }
    !matches!(
        &block.term,
        Some(Terminator::Br { cond, .. }) if *cond == dest
    )
}


/// Each block's compare its `Br` branches on, by block index, when nothing
/// else reads it (it is emitted fused into the branch). One pass over the
/// function: a scan per block made emit quadratic in its size (#1393).
pub(super) fn term_cmp_dests(func: &MirFunc) -> Vec<Option<ValueId>> {
    let mut read: HashSet<ValueId> = HashSet::new();
    let mut branch_on: HashMap<ValueId, Vec<BlockId>> = HashMap::new();
    for b in &func.blocks {
        for inst in &b.insts {
            read.extend(inst.operands());
        }
        match &b.term {
            Some(Terminator::Br { cond, .. }) => branch_on.entry(*cond).or_default().push(b.id),
            Some(Terminator::Return { lo, hi }) => read.extend(lo.iter().chain(hi.iter()).copied()),
            Some(Terminator::JumpIfMatch { scrutinee, payloads, .. }) => {
                read.insert(*scrutinee);
                read.extend(payloads.iter().copied());
            }
            _ => {}
        }
    }
    func.blocks
        .iter()
        .map(|b| {
            let dest = block_cmp_dest(b)?;
            let elsewhere = read.contains(&dest)
                || branch_on.get(&dest).is_some_and(|bs| bs.iter().any(|&o| o != b.id));
            (!elsewhere).then_some(dest)
        })
        .collect()
}

/// The `Cmp` of `block` its `Br` branches on.
fn block_cmp_dest(block: &super::func::MirBlock) -> Option<ValueId> {
    let Terminator::Br { cond, .. } = block.term.as_ref()? else {
        return None;
    };
    block.insts.iter().find_map(|inst| match inst {
        MirInst::Cmp { dest, .. } if dest == cond => Some(*dest),
        _ => None,
    })
}

pub(super) struct EmitBrCondArgs<'args> {
    out: &'args mut Vec<IlOp>,
    block: &'args super::func::MirBlock,
    func: &'args MirFunc,
    plan: &'args ConvoyPlan,
    regs: &'args [u8],
    cond: ValueId,
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

pub(super) fn emit_br_cond(args: EmitBrCondArgs<'_>) -> Result<(), LowerError> {
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
        if plan.needs_slot(*lhs)
            && let Some(imm) = tree_i16(func, plan, *rhs)
        {
            out.push(IlOp::BinSlotImm {
                op: stack_cmp_op(*op, *ty)? as u8,
                slot: regs[lhs.index()],
                imm,
                loc,
            });
            return Ok(());
        }
        // Either operand may be slotless (a constant `destprop` moved in
        // from a folded phi): rebuild it on the stack instead of loading a
        // register it never got.
        for v in [*lhs, *rhs] {
            if plan.needs_slot(v) {
                out.push(IlOp::Load {
                    slot: u32::from(regs[v.index()]),
                    loc,
                });
            } else {
                emit_stack_value(EmitStackValueArgs {
                    out,
                    stacked: &mut Vec::new(),
                    v,
                    func,
                    plan,
                    regs,
                    pool,
                    loc,
                })?;
            }
        }
        out.push(IlOp::Bin {
            op: stack_cmp_op(*op, *ty)?,
            loc,
        });
        return Ok(());
    }
    if plan.needs_slot(cond) {
        // A 0/1 bool register (`a < b || c == d`) branches as `slot != 0`:
        // fuse-select makes that one `BinSlotImmJmpf` (and packs it into
        // a `DenseBinJmpf` behind the `DenseBin` that computed it) where
        // `LOAD; JMPF` took two dispatches and left the dense streak.
        if canonical_bool_in(block, cond, 2) {
            out.push(IlOp::BinSlotImm {
                op: Instruction::NEQ as u8,
                slot: regs[cond.index()],
                imm: 0,
                loc,
            });
            return Ok(());
        }
        out.push(IlOp::Load {
            slot: u32::from(regs[cond.index()]),
            loc,
        });
        return Ok(());
    }
    emit_stack_value(EmitStackValueArgs {
        out,
        stacked: &mut Vec::new(),
        v: cond,
        func,
        plan,
        regs,
        pool,
        loc,
    })
}

/// `v` is a 0 / 1 word: a compare, or `&` / `|` / `^` of such words,
/// defined in `block` (`depth` bounds the walk). `JMPF` tests the low byte
/// only, so a branch may compare the whole word with 0 just for these.
fn canonical_bool_in(block: &super::func::MirBlock, v: ValueId, depth: u32) -> bool {
    let Some(def) = block.insts.iter().rev().find(|inst| inst.dest() == v) else {
        return false;
    };
    match def {
        MirInst::Cmp { .. } => true,
        MirInst::Bin {
            op: MirBinOp::BitAnd | MirBinOp::BitOr | MirBinOp::Xor,
            lhs,
            rhs,
            ..
        } if depth > 0 => {
            canonical_bool_in(block, *lhs, depth - 1) && canonical_bool_in(block, *rhs, depth - 1)
        }
        _ => false,
    }
}

pub(super) fn emit_cond_jumps(
    out: &mut Vec<IlOp>,
    func: &MirFunc,
    from: BlockId,
    taken: BlockId,
    not_taken: BlockId,
    block_lab: &[Label],
    loc: DebugLoc,
) {
    if is_fallthrough(func, from, not_taken) {
        out.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfTrue,
            target: block_lab[taken.index()],
            loc,
            hint: Default::default(),
        });
        return;
    }
    out.push(IlOp::Jump {
        kind: IlJumpKind::JumpIfFalse,
        target: block_lab[not_taken.index()],
        loc,
        hint: Default::default(),
    });
    if !is_fallthrough(func, from, taken) {
        out.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: block_lab[taken.index()],
            loc,
            hint: Default::default(),
        });
    }
}

/// Drop identity `DenseMove` (COI-383 S6). Runs before `DenseBin2` packing.
fn coalesce_dense_moves(ops: &mut Vec<IlOp>) {
    drop_identity_dense_moves(ops);
}

fn drop_identity_dense_moves(ops: &mut Vec<IlOp>) {
    ops.retain(|op| match op {
        IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::DenseMove => {
            let (d, s) = byte.dense_move_parts();
            d != s
        }
        _ => true,
    });
}

/// Pack adjacent [`Instruction::DenseBin`] into [`Instruction::DenseBin2`].
/// Sequential IEEE eval of both ops; no FMA / reassoc. Labels and other ops
/// are barriers. The payload word stays `DenseBin` so a jump cannot land in
/// the middle of a pack (labels already split blocks).
fn pack_chained_dense_bin(ops: &mut [IlOp]) {
    let mut i = 0;
    while i + 1 < ops.len() {
        let first = match &ops[i] {
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::DenseBin => Some(*byte),
            _ => None,
        };
        let second_is_bin = matches!(
            &ops[i + 1],
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::DenseBin
        );
        if first.is_none() || !second_is_bin {
            i += 1;
            continue;
        }
        let loc = ops[i].loc();
        let packed = Byte::new(Instruction::DenseBin2)
            .with_operand_u32(first.expect("DenseBin").operand_u32());
        ops[i] = IlOp::from_plain_byte(packed, loc);
        i += 2;
    }
}

/// Pack [`Instruction::DenseBin`] immediately followed by a reconstruct that
/// fuse-select turns into `*Jmpf` (COI-377 S1). The jump stays a typed
/// `IlOp` so label resolution / invert still run; the VM consumes the fused
/// jump as the payload word (`stream_width` 2).
fn pack_dense_bin_jmpf(ops: &mut [IlOp]) {
    let mut i = 0;
    while i + 1 < ops.len() {
        // A `DenseBin2`'s payload word must stay `DenseBin`: the VM reads it
        // as the second bin, and a `DenseBinJmpf` there would hide the jump
        // behind it from the GC-map pass (a rotated latch, #1867).
        if matches!(&ops[i], IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::DenseBin2) {
            i += 2;
            continue;
        }
        let first = match &ops[i] {
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::DenseBin => Some(*byte),
            _ => None,
        };
        if first.is_none() || !dense_bin_followed_by_cond_jmp(ops, i) {
            i += 1;
            continue;
        }
        let loc = ops[i].loc();
        let packed = Byte::new(Instruction::DenseBinJmpf)
            .with_operand_u32(first.expect("DenseBin").operand_u32());
        ops[i] = IlOp::from_plain_byte(packed, loc);
        i += 1;
    }
}

/// Pack [`Instruction::DenseIndex`] immediately followed by a reconstruct that
/// fuse-select turns into `*Jmpf` (COI-379 S4). Same stream-width lesson as
/// [`Instruction::DenseBinJmpf`]: the jump stays a typed `IlOp`; the VM
/// consumes it as the payload word.
fn pack_dense_index_jmpf(ops: &mut [IlOp]) {
    let mut i = 0;
    while i + 1 < ops.len() {
        let first = match &ops[i] {
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::DenseIndex => Some(*byte),
            _ => None,
        };
        if first.is_none() || !dense_bin_followed_by_cond_jmp(ops, i) {
            i += 1;
            continue;
        }
        let loc = ops[i].loc();
        let packed = Byte::new(Instruction::DenseIndexJmpf)
            .with_operand_u32(first.expect("DenseIndex").operand_u32());
        ops[i] = IlOp::from_plain_byte(packed, loc);
        i += 1;
    }
}

fn dense_bin_followed_by_cond_jmp(ops: &[IlOp], i: usize) -> bool {
    dense_bin_slot_slot_jmp(ops, i) || dense_bin_slot_imm_jmp(ops, i)
}

fn cond_jump_fusable(op: &IlOp) -> bool {
    match op {
        IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue,
            hint,
            ..
        } => !hint.blocks_cmp_jmp_fuse(),
        _ => false,
    }
}

fn dense_bin_slot_slot_jmp(ops: &[IlOp], i: usize) -> bool {
    if i + 4 >= ops.len() {
        return false;
    }
    matches!(&ops[i + 1], IlOp::Load { .. })
        && matches!(&ops[i + 2], IlOp::Load { .. } | IlOp::Dup { .. })
        && matches!(&ops[i + 3], IlOp::Bin { op, .. } if is_jmpf_cond_op(*op))
        && cond_jump_fusable(&ops[i + 4])
}

fn dense_bin_slot_imm_jmp(ops: &[IlOp], i: usize) -> bool {
    if i + 2 >= ops.len() {
        return false;
    }
    matches!(
        &ops[i + 1],
        IlOp::BinSlotImm { op, .. } if is_jmpf_cond_op(Instruction::from(*op))
    ) && cond_jump_fusable(&ops[i + 2])
}

fn is_jmpf_cond_op(i: Instruction) -> bool {
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
            | Instruction::EQF
            | Instruction::NEQF
            | Instruction::GTF
            | Instruction::GEQF
            | Instruction::AND
            | Instruction::OR
            | Instruction::BITAND
            | Instruction::BITOR
            | Instruction::XOR
    )
}

pub(super) struct EmitInstArgs<'args> {
    pub(super) out: &'args mut Vec<IlOp>,
    pub(super) inst: &'args MirInst,
    pub(super) func: &'args MirFunc,
    pub(super) regs: &'args [u8],
    pub(super) scratch: u8,
    pub(super) pool: &'args mut Vec<u64>,
    pub(super) loc: DebugLoc,
    pub(super) across_alloc: bool,
}

pub(super) fn emit_inst(args: EmitInstArgs<'_>) -> Result<(), LowerError> {
    let EmitInstArgs {
        out,
        inst,
        func,
        regs,
        scratch,
        pool,
        loc,
        across_alloc,
    } = args;

    let byte = |b: Byte| IlOp::from_plain_byte(b, loc);
    match inst {
        MirInst::Const { dest, c } => {
            out.push(emit_const(*c, regs[dest.index()], pool, loc)?);
        }
        MirInst::Bin {
            dest,
            op,
            ty,
            lhs,
            rhs,
        } => {
            let kind = bin_kind(*op, *ty)?;
            out.push(byte(
                Byte::new(Instruction::DenseBin).with_dense_abc(
                    kind,
                    regs[dest.index()],
                    regs[lhs.index()],
                    regs[rhs.index()],
                ),
            ));
        }
        MirInst::Cmp {
            dest,
            op,
            ty,
            lhs,
            rhs,
        } => {
            let kind = cmp_kind(*op, *ty)?;
            out.push(byte(
                Byte::new(Instruction::DenseCmp).with_dense_abc(
                    kind,
                    regs[dest.index()],
                    regs[lhs.index()],
                    regs[rhs.index()],
                ),
            ));
        }
        MirInst::ByteScan { dest, from, bound, inclusive, backward, streams, scalars, expr } => {
            let body = byte_scan_body(expr, streams, scalars, regs)
                .ok_or_else(|| LowerError::Refused("byte scan body too large".into()))?;
            let mut flag = if *inclusive { common::simd::SCAN_INCLUSIVE } else { 0 };
            if *backward {
                flag |= common::simd::SCAN_BACKWARD;
            }
            out.push(byte(Byte::new(Instruction::VScan).with_dense_abc(
                body.len() as u8 | flag,
                regs[dest.index()],
                regs[from.index()],
                regs[bound.index()],
            )));
            out.extend(body.into_iter().map(byte));
        }
        MirInst::Unary { dest, op, src } => {
            let kind = match (*op, func.ty(*src)) {
                (MirUnaryOp::Not, _) => dense::UNARY_NOT,
                (MirUnaryOp::Neg, t) if t.is_float() => dense::UNARY_FNEG,
                (MirUnaryOp::Neg, _) => dense::UNARY_NEG,
            };
            out.push(byte(
                Byte::new(Instruction::DenseUnary).with_dense_unary(
                    kind,
                    regs[dest.index()],
                    regs[src.index()],
                ),
            ));
        }
        MirInst::Cast {
            dest,
            kind,
            src,
            ..
        } => {
            let k = match kind {
                MirCastKind::IntToFloat => dense::CAST_I2F,
                MirCastKind::FloatToInt => dense::CAST_F2I,
                // The same word: a move, or nothing in place.
                MirCastKind::Word => {
                    let (d, s) = (regs[dest.index()], regs[src.index()]);
                    if d != s {
                        out.push(move_op(d, s));
                    }
                    return Ok(());
                }
            };
            out.push(byte(
                Byte::new(Instruction::DenseCast).with_dense_unary(
                    k,
                    regs[dest.index()],
                    regs[src.index()],
                ),
            ));
        }
        MirInst::Phi { .. } => {}
        MirInst::HostInvoke {
            dest,
            native_id,
            layout,
            args,
        } => {
            // `s[i]` reads the byte in place (minor 45): no ABI edge.
            if let [string, index] = args.as_slice()
                && super::host_allow::host_edge_spec_by_name("string_byte_at").is_some_and(|s| s.id == *native_id)
            {
                out.push(byte(Byte::new(Instruction::DenseIndex).with_dense_abc(
                    dense::HEAP_STR_BYTE,
                    regs[dest.index()],
                    regs[string.index()],
                    regs[index.index()],
                )));
                return Ok(());
            }
            // ABI edge: native id + DensePush args + HostInvoke + StorePop dest.
            out.push(IlOp::Const {
                imm: i32::from(*native_id),
                loc,
            });
            emit_dense_push(out, args, regs, scratch, loc)?;
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
        // Refused up front (`emit_dense`).
        MirInst::Ffi { .. } => {
            return Err(LowerError::Refused("dense emit refuses FFI".into()));
        }
        MirInst::CallIndirect { dest, callee, args, dicts } => {
            // The arguments, then the function value on top.
            let words: Vec<ValueId> = args.iter().copied().chain([*callee]).collect();
            emit_dense_push(out, &words, regs, scratch, loc)?;
            out.push(byte(Byte::new(Instruction::CallIndirect).with_operand_u32(call_indirect_operand(args.len(), *dicts))));
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
            emit_dense_push(out, args, regs, scratch, loc)?;
            let ret_words = crate::layout::ret_words_from_hi(*dest_hi);
            out.push(IlOp::Entry {
                kind: crate::il::EntryKind::Call,
                arity: args.len() as u32,
                target: *target,
                loc,
                ret_words,
            });
            store_call_dests(out, *dest, *dest_hi, regs, loc);
        }
        MirInst::Index {
            dest,
            array,
            index,
            unchecked,
        } => {
            let flags = if *unchecked {
                dense::HEAP_UNCHECKED
            } else {
                0
            };
            out.push(byte(
                Byte::new(Instruction::DenseIndex).with_dense_abc(
                    flags,
                    regs[dest.index()],
                    regs[array.index()],
                    regs[index.index()],
                ),
            ));
        }
        MirInst::StoreIndex {
            dest,
            array,
            index,
            value,
            unchecked,
        } => {
            let d = regs[dest.index()];
            let v = regs[value.index()];
            if d != v {
                out.push(byte(
                    Byte::new(Instruction::DenseMove).with_dense_move(d, v),
                ));
            }
            let flags = if *unchecked {
                dense::HEAP_UNCHECKED
            } else {
                0
            };
            out.push(byte(
                Byte::new(Instruction::DenseStoreIndex).with_dense_abc(
                    flags,
                    d,
                    regs[array.index()],
                    regs[index.index()],
                ),
            ));
        }
        MirInst::ArrayLen { dest, array } => {
            out.push(byte(
                Byte::new(Instruction::DenseArrayLen)
                    .with_dense_move(regs[dest.index()], regs[array.index()]),
            ));
        }
        MirInst::ResumeCoro {
            dest,
            handle,
            send,
        } => {
            if let Some(s) = send {
                out.push(IlOp::Load {
                    slot: u32::from(regs[s.index()]),
                    loc,
                });
            }
            out.push(IlOp::Load {
                slot: u32::from(regs[handle.index()]),
                loc,
            });
            let has_send = u32::from(send.is_some());
            out.push(byte(
                Byte::new(Instruction::ResumeCoro).with_operand_u32(has_send),
            ));
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Yield {
            dest,
            value,
            from,
            receive,
        } => {
            out.push(IlOp::Load {
                slot: u32::from(regs[value.index()]),
                loc,
            });
            let op = if *from { Instruction::YieldFromCoro } else { Instruction::YieldCoro };
            out.push(byte(Byte::new(op)));
            // The VM pushes the sent word only for a store right after.
            if *receive {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
        }
        MirInst::DoneCoro { dest, handle } => {
            out.push(IlOp::Load {
                slot: u32::from(regs[handle.index()]),
                loc,
            });
            out.push(byte(Byte::new(Instruction::DoneCoro)));
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::LoadStatic { dest, slot } => {
            out.push(byte(Byte::new(Instruction::LoadStatic).with_operand_u32(*slot)));
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::StoreStatic { slot, value, .. } => {
            out.push(IlOp::Load {
                slot: u32::from(regs[value.index()]),
                loc,
            });
            out.push(byte(Byte::new(Instruction::StoreStatic).with_operand_u32(*slot)));
        }
        // Emitted where it stands (`cleared_words`).
        MirInst::ClearWord { .. } | MirInst::KeepAlive { .. } => {}
        // A cleanup pad's homes: the LIR tier only (`emit_lir`).
        MirInst::HomeLoad { .. } | MirInst::HomeStore { .. } => {
            return Err(LowerError::Refused("homes are LIR-only".into()));
        }
        MirInst::ArrayPush {
            dest,
            array,
            value,
        } => {
            if !across_alloc {
                return Err(LowerError::Refused(
                    "dense emit refuses ArrayPush without GC maps (B6)".into(),
                ));
            }
            out.push(byte(
                Byte::new(Instruction::DenseArrayPush).with_dense_abc(
                    0,
                    regs[dest.index()],
                    regs[array.index()],
                    regs[value.index()],
                ),
            ));
        }
        MirInst::MatchPayload { dest, scrutinee, .. } => {
            let st = func.ty(*scrutinee);
            if !matches!(st, MirTy::NicheOpt | MirTy::NicheRes) {
                return Err(LowerError::Refused(
                    "dense MatchPayload is niche-only (boxed stays LIR)".into(),
                ));
            }
            let d = regs[dest.index()];
            let s = regs[scrutinee.index()];
            if d != s {
                out.push(move_op(d, s));
            }
        }
        MirInst::HeapFieldLoad { dest, object, index } => {
            emit_dense_field_load(out, regs[dest.index()], regs[object.index()], *index, loc)?;
        }
        MirInst::HeapFieldStore {
            dest,
            object,
            value,
            index,
        } => {
            let d = regs[dest.index()];
            let v = regs[value.index()];
            if d != v {
                out.push(move_op(d, v));
            }
            emit_dense_field_store(out, d, regs[object.index()], *index, loc)?;
        }
        MirInst::Alloc { dest, kind, elems, kinds: recorded } => {
            if !across_alloc {
                return Err(LowerError::Refused(
                    "dense emit refuses Alloc/GcBarrier without GC maps (S2c)".into(),
                ));
            }
            // `DenseMake` has no array element kind: a stamped array is made
            // on the stack as HIR emission makes it.
            let stamped_array = matches!(kind, MirAllocKind::Array) && *recorded != 0;
            if let Some(make_kind) = dense_make_kind(*kind)?.filter(|_| !stamped_array) {
                let arity = u8::try_from(elems.len())
                    .map_err(|_| LowerError::Refused("DenseMake arity".into()))?;
                let mut slots: Vec<u8> = elems.iter().map(|e| regs[e.index()]).collect();
                // `elems` is push order; `MakeEnum` pops, so payload[0] is the
                // last push. DenseMake reads declaration order.
                if matches!(kind, MirAllocKind::Enum { .. }) {
                    slots.reverse();
                }
                let base = gather_base(out, &slots, scratch, loc)?;
                // Declaration-order word kinds from the element types.
                let kinds = match kind {
                    _ if *recorded != 0 => *recorded,
                    _ if func.opaque_words => 0,
                    MirAllocKind::Enum { .. } => common::pack_word_kinds(
                        elems.iter().rev().map(|e| mir_word_kind(func.ty(*e))),
                    ),
                    MirAllocKind::Tuple => {
                        common::pack_word_kinds(elems.iter().map(|e| mir_word_kind(func.ty(*e))))
                    }
                    _ => 0,
                };
                if kinds != 0 {
                    let idx = intern_pool(pool, common::pack_dense_make_k(base, arity, kinds))?;
                    out.push(byte(
                        Byte::new(Instruction::DenseMakeK).with_dense_make_k(
                            make_kind,
                            regs[dest.index()],
                            idx,
                        ),
                    ));
                } else {
                    out.push(byte(
                        Byte::new(Instruction::DenseMake).with_dense_abc(
                            make_kind,
                            regs[dest.index()],
                            arity,
                            base,
                        ),
                    ));
                }
            } else if let Some(live) = object_make_dest_reg(*kind, *dest, func, regs) {
                if let Some(op) = dense_make_object(*kind, live, loc)? {
                    out.push(op);
                    let alloc_r = regs[dest.index()];
                    if alloc_r != live {
                        out.push(move_op(alloc_r, live));
                    }
                } else {
                    emit_dense_push(out, elems, regs, scratch, loc)?;
                    out.extend(il_for_alloc(*kind, elems.len() as u32, *recorded, loc)?);
                    out.push(IlOp::StorePop {
                        slot: u32::from(live),
                        loc,
                    });
                }
            } else {
                emit_dense_push(out, elems, regs, scratch, loc)?;
                out.extend(il_for_alloc(*kind, elems.len() as u32, *recorded, loc)?);
                if dead_drop_alloc(func, *kind, *dest) {
                    out.push(IlOp::Pop { loc });
                } else {
                    out.push(IlOp::StorePop {
                        slot: u32::from(regs[dest.index()]),
                        loc,
                    });
                }
            }
        }
        MirInst::GcBarrier { dest, .. } => {
            if !across_alloc {
                return Err(LowerError::Refused(
                    "dense emit refuses Alloc/GcBarrier without GC maps (S2c)".into(),
                ));
            }
            if let Some(obj) = paired_alloc_dest(func, *dest)
                && regs[dest.index()] != regs[obj.index()] {
                    out.push(IlOp::Load {
                        slot: u32::from(regs[obj.index()]),
                        loc,
                    });
                    out.push(IlOp::StorePop {
                        slot: u32::from(regs[dest.index()]),
                        loc,
                    });
                }
        }
        MirInst::Deopt { .. } => {}
        MirInst::String { dest, idx } => {
            // Field-name keys (GetField/SetField). Not FORMAT/PRINT (Q9 R1).
            out.push(IlOp::String { idx: *idx, loc });
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::CodePtr { dest, target } => {
            out.push(code_ptr_op(*target, loc));
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        // Table ops at the stack edge, like HostInvoke: push operands, run
        // the shipped opcode, store the result.
        MirInst::Format { dest, fmt, args } => {
            let words: Vec<ValueId> = std::iter::once(*fmt).chain(args.iter().copied()).collect();
            emit_dense_push(out, &words, regs, scratch, loc)?;
            out.push(IlOp::from_plain_byte(
                Byte::new(Instruction::FORMAT).with_operand_u32(args.len() as u32),
                loc,
            ));
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Stringify { dest, src } => {
            emit_dense_push(out, &[*src], regs, scratch, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::STRINGIFY), loc));
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::Print { src, .. } => {
            emit_dense_push(out, &[*src], regs, scratch, loc)?;
            out.push(IlOp::Print { loc });
        }
        MirInst::Panic { msg, .. } => {
            emit_dense_push(out, &[*msg], regs, scratch, loc)?;
            out.push(IlOp::from_plain_byte(Byte::new(Instruction::Panic), loc));
        }
        MirInst::Unbox { dest, src, tag } => {
            emit_dense_push(out, &[*src], regs, scratch, loc)?;
            out.push(IlOp::UnboxValue { tag: *tag, loc });
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        MirInst::StrEq { dest, lhs, rhs, ne } => {
            // `DenseCmp` compares words; VM `EQ` compares strings by content.
            emit_dense_push(out, &[*lhs, *rhs], regs, scratch, loc)?;
            out.push(IlOp::Bin {
                op: if *ne { Instruction::NEQ } else { Instruction::EQ },
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

struct EmitTermArgs<'args> {
    out: &'args mut Vec<IlOp>,
    stacked: &'args mut Vec<ValueId>,
    block: &'args super::func::MirBlock,
    func: &'args MirFunc,
    plan: &'args ConvoyPlan,
    regs: &'args [u8],
    scratch: u8,
    block_lab: &'args [Label],
    next_label: &'args mut u32,
    reserved: &'args HashSet<u32>,
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
    /// Registers zeroed after an edge's φ copies ([`entry_clears`]).
    clears: &'args HashMap<(BlockId, BlockId), Vec<u8>>,
    /// Loop headers a back edge copies its compare-branch into ([`rotatable_headers`]).
    rotate: &'args [bool],
}

fn emit_term(args: EmitTermArgs<'_>) -> Result<(), LowerError> {
    let EmitTermArgs {
        out,
        stacked,
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
        rotate,
    } = args;

    let Some(term) = &block.term else {
        return Err(LowerError::Refused("missing terminator".into()));
    };
    let clears = |to: BlockId| clears.get(&(block.id, to)).map(Vec::as_slice).unwrap_or(&[]);
    match term {
        Terminator::Jump { dest } => {
            emit_phi_moves(out, func, block.id, *dest, regs, scratch);
            for &r in clears(*dest) {
                push_clear_word(out, r);
            }
            if rotate[dest.index()] && emitted_before(func, *dest, block.id) {
                // Loop inversion (#1867): the back edge tests the header's
                // condition itself and jumps straight into the body, so an
                // iteration skips the header dispatch. The compare fuses with
                // the latch's `DenseBin` into one `DenseBinJmpf`.
                let header = func.block(*dest);
                let Some(Terminator::Br { cond, taken, not_taken }) = &header.term else {
                    unreachable!("rotatable header ends in Br");
                };
                let loc = func.term_loc(header.id);
                emit_br_cond(EmitBrCondArgs {
                    out,
                    block: header,
                    func,
                    plan,
                    regs,
                    cond: *cond,
                    pool,
                    loc,
                })?;
                out.push(IlOp::Jump {
                    kind: IlJumpKind::JumpIfTrue,
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
                return Ok(());
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
            if t_moves.is_empty() && f_moves.is_empty() && t_clears.is_empty() && f_clears.is_empty() {
                emit_cond_jumps(
                    out,
                    func,
                    block.id,
                    *taken,
                    *not_taken,
                    block_lab,
                    loc,
                );
            } else {
                let f_lab = take_label(next_label, reserved);
                out.push(IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    target: f_lab,
                    loc,
                    hint: Default::default(),
                });
                for (d, s) in t_moves {
                    out.push(move_op(d, s));
                }
                for &r in t_clears {
                    push_clear_word(out, r);
                }
                // f_lab is the next op; taken must JMP or true fallthrough
                // lands on the false phi moves (encode_frame if-chain).
                out.push(IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: block_lab[taken.index()],
                    loc,
                    hint: Default::default(),
                });
                out.push(IlOp::Label(f_lab));
                for (d, s) in f_moves {
                    out.push(move_op(d, s));
                }
                for &r in f_clears {
                    push_clear_word(out, r);
                }
                out.push(IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: block_lab[not_taken.index()],
                    loc,
                    hint: Default::default(),
                });
            }
        }
        Terminator::Return { lo, hi } => {
            if let Some(v) = lo {
                if let Some(
                    last @ MirInst::Call {
                        dest,
                        dest_hi,
                        target,
                        args,
                    },
                ) = block.insts.last()
                    && *dest == *v && *dest_hi == *hi
                    && is_tail_call_inst(func, block, last) {
                        emit_call(EmitCallArgs {
                            out,
                            stacked,
                            kind: crate::il::EntryKind::TailCall,
                            dest: *dest,
                            dest_hi: *dest_hi,
                            target: *target,
                            args,
                            func,
                            plan,
                            regs,
                            scratch,
                            pool,
                            loc,
                        })?;
                        return Ok(());
                    }
                emit_stack_value(EmitStackValueArgs {
                    out,
                    stacked,
                    v: *v,
                    func,
                    plan,
                    regs,
                    pool,
                    loc,
                })?;
            } else {
                out.push(IlOp::Const { imm: 0, loc });
            }
            if let Some(h) = hi {
                // `return v, v` (`None` as `0, 0`): the low word is the
                // value on top, which `emit_stack_value` would reuse
                // instead of pushing it a second time.
                if *lo == Some(*h) {
                    out.push(IlOp::Dup { loc });
                } else {
                    emit_stack_value(EmitStackValueArgs {
                        out,
                        stacked,
                        v: *h,
                        func,
                        plan,
                        regs,
                        pool,
                        loc,
                    })?;
                }
            }
            out.push(IlOp::Return {
                loc,
                ret_words: crate::layout::ret_words_from_hi(*hi),
            });
        }
        Terminator::Unreachable => {
            out.push(IlOp::Halt { loc });
        }
        Terminator::JumpIfMatch {
            scrutinee,
            tag,
            payloads,
            taken,
            not_taken,
        } => {
            emit_dense_jump_if_match(EmitDenseJumpIfMatchArgs {
                out,
                stacked,
                func,
                plan,
                block,
                scrutinee: *scrutinee,
                tag: *tag,
                payloads,
                taken: *taken,
                not_taken: *not_taken,
                regs,
                scratch,
                block_lab,
                next_label,
                reserved,
                pool,
                loc,
            })?;
        }
    }
    Ok(())
}

fn emit_phi_moves(
    out: &mut Vec<IlOp>,
    func: &MirFunc,
    pred: BlockId,
    succ: BlockId,
    regs: &[u8],
    scratch: u8,
) {
    for (d, s) in phi_moves(func, pred, succ, regs, scratch) {
        out.push(move_op(d, s));
    }
}

fn move_op(dest: u8, src: u8) -> IlOp {
    IlOp::byte(Byte::new(Instruction::DenseMove).with_dense_move(dest, src))
}

pub(super) fn emit_const(
    c: MirConst,
    dest: u8,
    pool: &mut Vec<u64>,
    loc: DebugLoc,
) -> Result<IlOp, LowerError> {
    let byte = |b: Byte| IlOp::from_plain_byte(b, loc);
    match c {
        MirConst::I64(v) => {
            if let Ok(imm) = i16::try_from(v) {
                Ok(byte(Byte::new(Instruction::DenseConst).with_dense_const(
                    dense::TY_I64,
                    dest,
                    imm as u16,
                    false,
                )))
            } else {
                let idx = intern_pool(pool, v as u64)?;
                Ok(byte(Byte::new(Instruction::DenseConst).with_dense_const(
                    dense::TY_I64,
                    dest,
                    idx,
                    true,
                )))
            }
        }
        MirConst::F64(bits) => {
            let idx = intern_pool(pool, bits)?;
            Ok(byte(Byte::new(Instruction::DenseConst).with_dense_const(
                dense::TY_F64,
                dest,
                idx,
                true,
            )))
        }
        MirConst::Bool(v) => Ok(byte(Byte::new(Instruction::DenseConst).with_dense_const(
            dense::TY_BOOL,
            dest,
            u16::from(v),
            false,
        ))),
    }
}

struct EmitDenseJumpIfMatchArgs<'args> {
    out: &'args mut Vec<IlOp>,
    stacked: &'args mut Vec<ValueId>,
    func: &'args MirFunc,
    plan: &'args ConvoyPlan,
    block: &'args super::func::MirBlock,
    scrutinee: ValueId,
    tag: u32,
    payloads: &'args [ValueId],
    taken: BlockId,
    not_taken: BlockId,
    regs: &'args [u8],
    scratch: u8,
    block_lab: &'args [Label],
    next_label: &'args mut u32,
    reserved: &'args HashSet<u32>,
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

fn emit_dense_jump_if_match(args: EmitDenseJumpIfMatchArgs<'_>) -> Result<(), LowerError> {
    let EmitDenseJumpIfMatchArgs {
        out,
        stacked,
        func,
        plan,
        block,
        scrutinee,
        tag,
        payloads,
        taken,
        not_taken,
        regs,
        scratch,
        block_lab,
        next_label,
        reserved,
        pool,
        loc,
    } = args;

    let st = func.ty(scrutinee);
    if !matches!(st, MirTy::NicheOpt | MirTy::NicheRes) {
        return emit_boxed_jump_if_match(EmitBoxedJumpIfMatchArgs {
            out,
            stacked,
            func,
            plan,
            block,
            scrutinee,
            tag,
            payloads,
            taken,
            not_taken,
            regs,
            scratch,
            block_lab,
            pool,
            loc,
        });
    }
    if tag > 1 {
        return Err(LowerError::Refused(
            "dense JumpIfMatch tag > 1 (keep fuse-IL)".into(),
        ));
    }
    let t_moves = phi_moves(func, block.id, taken, regs, scratch);
    let f_moves = phi_moves(func, block.id, not_taken, regs, scratch);
    out.push(IlOp::Load {
        slot: u32::from(regs[scrutinee.index()]),
        loc,
    });
    match st {
        MirTy::NicheOpt => {
            // None = 0 (tag 0), Some = nonzero pointer (tag 1).
            out.push(IlOp::Const { imm: 0, loc });
            out.push(IlOp::Bin {
                op: Instruction::EQ,
                loc,
            });
        }
        MirTy::NicheRes => {
            // Err = ptr | 1 (tag 1), Ok = aligned pointer (tag 0).
            out.push(IlOp::Const { imm: 1, loc });
            out.push(IlOp::Bin {
                op: Instruction::BITAND,
                loc,
            });
        }
        _ => unreachable!(),
    }
    // Cond true: NicheOpt None / NicheRes Err.
    let cond_true_is_taken = match st {
        MirTy::NicheOpt => tag == 0,
        MirTy::NicheRes => tag == 1,
        _ => unreachable!(),
    };
    let (true_dest, false_dest, true_phi, false_phi) = if cond_true_is_taken {
        (taken, not_taken, t_moves, f_moves)
    } else {
        (not_taken, taken, f_moves, t_moves)
    };
    if true_phi.is_empty() && false_phi.is_empty() {
        emit_cond_jumps(out, func, block.id, true_dest, false_dest, block_lab, loc);
        return Ok(());
    }
    let f_lab = take_label(next_label, reserved);
    out.push(IlOp::Jump {
        kind: IlJumpKind::JumpIfFalse,
        target: f_lab,
        loc,
        hint: Default::default(),
    });
    for (d, s) in true_phi {
        out.push(move_op(d, s));
    }
    if !is_fallthrough(func, block.id, true_dest) {
        out.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: block_lab[true_dest.index()],
            loc,
            hint: Default::default(),
        });
    }
    out.push(IlOp::Label(f_lab));
    for (d, s) in false_phi {
        out.push(move_op(d, s));
    }
    out.push(IlOp::Jump {
        kind: IlJumpKind::Unconditional,
        target: block_lab[false_dest.index()],
        loc,
        hint: Default::default(),
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn seed_jim_taken_stack(
    out: &mut Vec<IlOp>,
    stacked: &mut Vec<ValueId>,
    func: &MirFunc,
    block: BlockId,
    preds: &[BlockId],
    plan: &ConvoyPlan,
    regs: &[u8],
    loc: DebugLoc,
) {
    for pred in preds {
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
        if payloads.len() > 1 || payloads.iter().any(|d| plan.needs_slot(*d)) {
            for dest in payloads.iter().rev() {
                out.push(IlOp::StorePop {
                    slot: u32::from(regs[dest.index()]),
                    loc,
                });
            }
            stacked.clear();
        } else {
            stacked.clear();
            stacked.extend(payloads.iter().copied());
        }
        return;
    }
}

/// Every pred reaches `block` as the miss edge of a `JumpIfMatch` on
/// `scrutinee`, which peeks and so leaves it on top of the stack.
pub(super) fn scrutinee_left_by_miss(func: &MirFunc, block: BlockId, scrutinee: ValueId) -> bool {
    left_by_miss(func, &func.preds_of(block), block, scrutinee)
}

/// [`scrutinee_left_by_miss`], given `block`'s preds.
fn left_by_miss(func: &MirFunc, preds: &[BlockId], block: BlockId, scrutinee: ValueId) -> bool {
    !preds.is_empty()
        && preds.iter().all(|p| {
            matches!(
                &func.block(*p).term,
                Some(Terminator::JumpIfMatch { scrutinee: s, not_taken, .. })
                    if *s == scrutinee && *not_taken == block
            )
        })
}

/// The miss edge of a `JumpIfMatch` on `scrutinee` into `not_taken` pops
/// the scrutinee itself: `not_taken` is also reached by a path that leaves
/// no scrutinee (a failed sub-pattern test of an earlier arm, `Op::Mul(a, 1)`
/// on `Mul(4, 2)`), so nothing there takes the word the miss left, and a
/// loop running the match grows the stack each pass (#1780).
pub(super) fn miss_edge_pops(func: &MirFunc, scrutinee: ValueId, not_taken: BlockId) -> bool {
    !scrutinee_left_by_miss(func, not_taken, scrutinee)
}

/// `block` is reached only by boxed `JumpIfMatch` misses, which leave the
/// scrutinee on the stack, takes no payload of it (a unit or wildcard last
/// arm, or a later arm's test that pushes the scrutinee again), and can run
/// again in the frame: the leftover is popped, or every pass grows the
/// stack.
pub(super) fn missed_scrutinee_unused(func: &MirFunc, preds: &[BlockId], block: BlockId) -> bool {
    missed_scrutinee(func, preds, block).is_some_and(|s| last_arm_payloads(func, block, s).is_empty()) && on_cycle(func, block)
}

/// `block` can run again in the same frame. Off a cycle, the word a miss
/// leaves goes when the frame returns.
fn on_cycle(func: &MirFunc, block: BlockId) -> bool {
    let succs = |b: BlockId| func.block(b).term.as_ref().map(Terminator::succs).unwrap_or_default();
    let mut seen = vec![false; func.blocks.len()];
    let mut work = succs(block);
    while let Some(b) = work.pop() {
        if b == block {
            return true;
        }
        if !std::mem::replace(&mut seen[b.index()], true) {
            work.extend(succs(b));
        }
    }
    false
}

/// The boxed scrutinee every pred of `block` leaves on the stack by
/// missing a `JumpIfMatch` on it.
fn missed_scrutinee(func: &MirFunc, preds: &[BlockId], block: BlockId) -> Option<ValueId> {
    let &pred = preds.first()?;
    let Some(Terminator::JumpIfMatch { scrutinee, not_taken, .. }) = &func.block(pred).term else {
        return None;
    };
    (*not_taken == block
        && !matches!(func.ty(*scrutinee), MirTy::NicheOpt | MirTy::NicheRes)
        && left_by_miss(func, preds, block, *scrutinee))
    .then_some(*scrutinee)
}

/// `block` only tests the next arm of the match whose miss reached it (no
/// instruction but that test's own payloads): the scrutinee the miss left
/// on the stack is the one it tests, as HIR emission's chained
/// `JumpIfMatch`.
pub(super) fn miss_chain_tos(func: &MirFunc, preds: &[BlockId], block: BlockId) -> Option<ValueId> {
    let s = missed_scrutinee(func, preds, block)?;
    let b = func.block(block);
    let Some(Terminator::JumpIfMatch { scrutinee, payloads, .. }) = &b.term else {
        return None;
    };
    (*scrutinee == s
        && b.insts.iter().all(|i| {
            i.is_phi() || matches!(i, MirInst::MatchPayload { dest, scrutinee: src, .. } if *src == s && payloads.contains(dest))
        }))
    .then_some(s)
}

struct EmitBoxedLastArmUnpackArgs<'args> {
    out: &'args mut Vec<IlOp>,
    stacked: &'args mut Vec<ValueId>,
    func: &'args MirFunc,
    block: BlockId,
    scrutinee: ValueId,
    plan: &'args ConvoyPlan,
    regs: &'args [u8],
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

fn emit_boxed_last_arm_unpack(args: EmitBoxedLastArmUnpackArgs<'_>) -> Result<(), LowerError> {
    let EmitBoxedLastArmUnpackArgs {
        out,
        stacked,
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
    // stack; otherwise (single-variant match) it may live in a register,
    // e.g. a `DenseIndex` result.
    if !scrutinee_left_by_miss(func, block, scrutinee) {
        emit_stack_value(EmitStackValueArgs {
            out,
            stacked,
            v: scrutinee,
            func,
            plan,
            regs,
            pool,
            loc,
        })?;
        stacked.pop();
    }
    out.push(IlOp::from_plain_byte(
        Byte::new(Instruction::Unpack).with_operand_u32(arity),
        loc,
    ));
    if group.len() > 1 || group.iter().any(|d| plan.needs_slot(*d)) {
        for dest in group.iter().rev() {
            out.push(IlOp::StorePop {
                slot: u32::from(regs[dest.index()]),
                loc,
            });
        }
        stacked.clear();
    } else {
        stacked.clear();
        stacked.extend(group);
    }
    Ok(())
}

struct EmitBoxedJumpIfMatchArgs<'args> {
    out: &'args mut Vec<IlOp>,
    stacked: &'args mut Vec<ValueId>,
    func: &'args MirFunc,
    plan: &'args ConvoyPlan,
    block: &'args super::func::MirBlock,
    scrutinee: ValueId,
    tag: u32,
    payloads: &'args [ValueId],
    taken: BlockId,
    not_taken: BlockId,
    regs: &'args [u8],
    scratch: u8,
    block_lab: &'args [Label],
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

fn emit_boxed_jump_if_match(args: EmitBoxedJumpIfMatchArgs<'_>) -> Result<(), LowerError> {
    let EmitBoxedJumpIfMatchArgs {
        out,
        stacked,
        func,
        plan,
        block,
        scrutinee,
        tag,
        payloads,
        taken,
        not_taken,
        regs,
        scratch,
        block_lab,
        pool,
        loc,
    } = args;

    if !phi_moves(func, block.id, taken, regs, scratch).is_empty()
        || !phi_moves(func, block.id, not_taken, regs, scratch).is_empty()
    {
        return Err(LowerError::Refused(
            "dense boxed JumpIfMatch + phi moves (keep fuse-IL)".into(),
        ));
    }
    emit_stack_value(EmitStackValueArgs {
        out,
        stacked,
        v: scrutinee,
        func,
        plan,
        regs,
        pool,
        loc,
    })?;
    out.push(IlOp::Jump {
        kind: IlJumpKind::JumpIfMatch {
            tag,
            arity: payloads.len() as u32,
        },
        target: block_lab[taken.index()],
        loc,
        hint: Default::default(),
    });
    if stacked.last() == Some(&scrutinee) {
        stacked.pop();
    } else {
        stacked.clear();
    }
    if miss_edge_pops(func, scrutinee, not_taken) {
        out.push(IlOp::Pop { loc });
    }
    if !is_fallthrough(func, block.id, not_taken) {
        out.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: block_lab[not_taken.index()],
            loc,
            hint: Default::default(),
        });
    }
    Ok(())
}

fn bin_kind(op: MirBinOp, ty: MirTy) -> Result<u8, LowerError> {
    let ty = match ty {
        // Bool words are 0/1, so `&` / `|` on i64 are exact.
        MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes | MirTy::Bool => MirTy::I64,
        other => other,
    };
    Ok(match (op, ty) {
        (MirBinOp::Add, MirTy::I64) => dense::IADD64,
        (MirBinOp::Sub, MirTy::I64) => dense::ISUB64,
        (MirBinOp::Mul, MirTy::I64) => dense::IMUL64,
        (MirBinOp::Div, MirTy::I64) => dense::IDIV64,
        (MirBinOp::Rem, MirTy::I64) => dense::IREM64,
        (MirBinOp::BitAnd, MirTy::I64) => dense::IAND64,
        (MirBinOp::BitOr, MirTy::I64) => dense::IOR64,
        (MirBinOp::Xor, MirTy::I64) => dense::IXOR64,
        (MirBinOp::Shl, MirTy::I64) => dense::ISHL64,
        (MirBinOp::Shr, MirTy::I64) => dense::ISHR64,
        (MirBinOp::Add, MirTy::F64) => dense::FADD64,
        (MirBinOp::Sub, MirTy::F64) => dense::FSUB64,
        (MirBinOp::Mul, MirTy::F64) => dense::FMUL64,
        (MirBinOp::Div, MirTy::F64) => dense::FDIV64,
        (MirBinOp::Rem, MirTy::F64) => dense::FREM64,
        _ => return Err(LowerError::Refused(format!("dense bin {op:?} {ty}"))),
    })
}

fn cmp_kind(op: MirCmpOp, ty: MirTy) -> Result<u8, LowerError> {
    let pred = match op {
        MirCmpOp::Lt => dense::CMP_LT,
        MirCmpOp::Le => dense::CMP_LE,
        MirCmpOp::Gt => dense::CMP_GT,
        MirCmpOp::Ge => dense::CMP_GE,
        MirCmpOp::Eq => dense::CMP_EQ,
        MirCmpOp::Ne => dense::CMP_NE,
    };
    let lane = match ty {
        MirTy::I64 | MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes => dense::CMP_I64,
        MirTy::F64 => dense::CMP_F64,
        _ => return Err(LowerError::Refused(format!("dense cmp {ty}"))),
    };
    Ok(dense::pack_cmp(lane, pred))
}

fn gather_window(func: &MirFunc, self_entry: Option<Label>) -> u8 {
    let mut n = 0u8;
    for b in &func.blocks {
        for inst in &b.insts {
            if is_tail_call_inst(func, b, inst) {
                continue;
            }
            let w = match inst {
                MirInst::Call { args, target, .. } if Some(*target) != self_entry => args.len(),
                MirInst::HostInvoke { args, .. } | MirInst::Ffi { args, .. } => args.len(),
                MirInst::CallIndirect { args, .. } => args.len() + 1,
                MirInst::Alloc { elems, .. } => elems.len(),
                // Format string plus args, pushed like call args.
                MirInst::Format { args, .. } => args.len() + 1,
                MirInst::Stringify { .. } | MirInst::Unbox { .. } | MirInst::Print { .. } | MirInst::Panic { .. } => 1,
                MirInst::StrEq { .. } => 2,
                _ => 0,
            };
            n = n.max(u8::try_from(w).unwrap_or(u8::MAX));
        }
    }
    n
}

fn gather_base(
    out: &mut Vec<IlOp>,
    slots: &[u8],
    scratch: u8,
    loc: DebugLoc,
) -> Result<u8, LowerError> {
    if slots.is_empty() {
        return Ok(0);
    }
    if slots.windows(2).all(|w| w[1] == w[0].saturating_add(1)) {
        return Ok(slots[0]);
    }
    for (i, &src) in slots.iter().enumerate() {
        let dest = scratch
            .checked_add(i as u8)
            .ok_or_else(|| LowerError::Refused("dense gather overflow".into()))?;
        if dest != src {
            out.push(IlOp::byte(
                Byte::new(Instruction::DenseMove).with_dense_move(dest, src),
            ));
        }
    }
    let _ = loc;
    Ok(scratch)
}

struct EmitCallArgs<'args> {
    out: &'args mut Vec<IlOp>,
    stacked: &'args mut Vec<ValueId>,
    kind: crate::il::EntryKind,
    dest: ValueId,
    dest_hi: Option<ValueId>,
    target: crate::il::Label,
    args: &'args [ValueId],
    func: &'args MirFunc,
    plan: &'args ConvoyPlan,
    regs: &'args [u8],
    scratch: u8,
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

fn emit_call(args: EmitCallArgs<'_>) -> Result<(), LowerError> {
    let EmitCallArgs {
        out,
        stacked,
        kind,
        dest,
        dest_hi,
        target,
        args,
        func,
        plan,
        regs,
        scratch,
        pool,
        loc,
    } = args;

    let ret_words = crate::layout::ret_words_from_hi(dest_hi);
    // TailCall (self or sibling): args on the operand stack, then jump.
    // Do not DensePush / convoy a foreign CALL dest as a self-return (B2).
    if kind == crate::il::EntryKind::TailCall {
        emit_args_on_stack(EmitArgsOnStackArgs {
            out,
            stacked,
            args,
            func,
            plan,
            regs,
            pool,
            loc,
        })?;
        out.push(IlOp::Entry {
            kind,
            arity: args.len() as u32,
            target,
            loc,
            ret_words,
        });
        return Ok(());
    }
    if !plan.is_self_call(target) {
        stacked.clear();
        emit_dense_push(out, args, regs, scratch, loc)?;
        out.push(IlOp::Entry {
            kind,
            arity: args.len() as u32,
            target,
            loc,
            ret_words,
        });
        store_or_stack_call(out, stacked, dest, dest_hi, plan, regs, loc);
        return Ok(());
    }
    emit_args_on_stack(EmitArgsOnStackArgs {
        out,
        stacked,
        args,
        func,
        plan,
        regs,
        pool,
        loc,
    })?;
    out.push(IlOp::Entry {
        kind,
        arity: args.len() as u32,
        target,
        loc,
        ret_words,
    });
    let arity = args.len();
    if stacked.len() >= arity {
        stacked.truncate(stacked.len() - arity);
    } else {
        stacked.clear();
    }
    if kind == crate::il::EntryKind::TailCall {
        return Ok(());
    }
    store_or_stack_call(out, stacked, dest, dest_hi, plan, regs, loc);
    Ok(())
}

/// Give the call or trapping op among `ops` (emitted for a value whose
/// loc is `value`, a variable's def site) the op's own loc `own`, so a panic
/// reports the op's line rather than the variable's (#841, #885).
/// [`restore_op_locs`] over one block's insts: `starts` holds where each
/// inst's ops begin, up to `end`.
pub(super) fn restore_block_op_locs(out: &mut [IlOp], starts: &[(usize, ValueId)], end: usize, func: &MirFunc) {
    for (i, &(start, v)) in starts.iter().enumerate() {
        if !func.op_locs.contains_key(&v) {
            continue;
        }
        let stop = starts.get(i + 1).map_or(end, |&(s, _)| s);
        restore_op_locs(&mut out[start..stop], func.loc_of(v), func.op_loc(v));
    }
}

pub(super) fn restore_op_locs(ops: &mut [IlOp], value: DebugLoc, own: DebugLoc) {
    if !own.is_known() || own == value {
        return;
    }
    let traps = |op: u8| {
        [Instruction::ADD, Instruction::SUB, Instruction::MUL, Instruction::DIV, Instruction::MOD, Instruction::Pow]
            .iter()
            .any(|&i| i as u8 == op)
    };
    for op in ops {
        let mine = match op {
            IlOp::Entry { kind, .. } => matches!(kind, crate::il::EntryKind::Call | crate::il::EntryKind::TailCall),
            IlOp::HostInvoke { .. } | IlOp::Index { .. } => true,
            IlOp::Bin { op, .. } => traps(*op as u8),
            IlOp::BinSlotImm { op, .. } | IlOp::BinSlotSlot { op, .. } => traps(*op),
            IlOp::Byte { byte, .. } => matches!(
                byte.bytecode(),
                Instruction::NEG | Instruction::CallIndirect | Instruction::HostInvoke | Instruction::HostInvokeNiche
            ),
            _ => false,
        };
        if mine && op.loc() == value {
            op.set_loc(own);
        }
    }
}

fn store_call_dests(
    out: &mut Vec<IlOp>,
    dest: ValueId,
    dest_hi: Option<ValueId>,
    regs: &[u8],
    loc: DebugLoc,
) {
    if let Some(hi) = dest_hi {
        out.push(IlOp::StorePop {
            slot: u32::from(regs[hi.index()]),
            loc,
        });
    }
    out.push(IlOp::StorePop {
        slot: u32::from(regs[dest.index()]),
        loc,
    });
}

fn store_or_stack_call(
    out: &mut Vec<IlOp>,
    stacked: &mut Vec<ValueId>,
    dest: ValueId,
    dest_hi: Option<ValueId>,
    plan: &ConvoyPlan,
    regs: &[u8],
    loc: DebugLoc,
) {
    let park = plan.needs_slot(dest) || dest_hi.is_some_and(|h| plan.needs_slot(h));
    if park {
        store_call_dests(out, dest, dest_hi, regs, loc);
        return;
    }
    stacked.push(dest);
    if let Some(hi) = dest_hi {
        stacked.push(hi);
    }
}

struct EmitArgsOnStackArgs<'args> {
    out: &'args mut Vec<IlOp>,
    stacked: &'args mut Vec<ValueId>,
    args: &'args [ValueId],
    func: &'args MirFunc,
    plan: &'args ConvoyPlan,
    regs: &'args [u8],
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

fn emit_args_on_stack(args: EmitArgsOnStackArgs<'_>) -> Result<(), LowerError> {
    let EmitArgsOnStackArgs {
        out,
        stacked,
        args,
        func,
        plan,
        regs,
        pool,
        loc,
    } = args;

    if !args.is_empty() && stacked.ends_with(args) {
        return Ok(());
    }
    // A pushed argument is spoken for: a later argument must not consume it
    // as an operand (`g(c, 1, c - 1)` would fold `c, 1` into the `SUB`), nor
    // reuse it as its own value (`f(x, x)`), so each one after the first
    // sees only what it pushes itself.
    let mut below = Vec::new();
    for (i, &a) in args.iter().enumerate() {
        if i > 0 {
            below.append(stacked);
        }
        emit_stack_value(EmitStackValueArgs {
            out,
            stacked,
            v: a,
            func,
            plan,
            regs,
            pool,
            loc,
        })?;
    }
    below.append(stacked);
    *stacked = below;
    Ok(())
}

struct EmitStackValueArgs<'args> {
    out: &'args mut Vec<IlOp>,
    stacked: &'args mut Vec<ValueId>,
    v: ValueId,
    func: &'args MirFunc,
    plan: &'args ConvoyPlan,
    regs: &'args [u8],
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

fn emit_stack_value(args: EmitStackValueArgs<'_>) -> Result<(), LowerError> {
    let EmitStackValueArgs {
        out,
        stacked,
        v,
        func,
        plan,
        regs,
        pool,
        loc,
    } = args;

    if stacked.last() == Some(&v) {
        return Ok(());
    }
    if plan.needs_slot(v) {
        out.push(IlOp::Load {
            slot: u32::from(regs[v.index()]),
            loc,
        });
        stacked.push(v);
        return Ok(());
    }
    let Some((bid, idx)) = plan.def[v.index()] else {
        return Err(LowerError::Refused("dense convoy undef".into()));
    };
    match &func.block(bid).insts[idx] {
        MirInst::Const { c, .. } => {
            push_const::<u16>(out, *c, pool, loc)?;
            stacked.push(v);
            Ok(())
        }
        MirInst::Bin {
            dest,
            op,
            ty,
            lhs,
            rhs,
        } => {
            emit_stack_bin(EmitStackBinArgs {
                out,
                stacked,
                op: *op,
                ty: *ty,
                lhs: *lhs,
                rhs: *rhs,
                dest: *dest,
                func,
                plan,
                regs,
                pool,
                loc,
            })
        }
        // A slotless call result is only valid while it is still TOS
        // (`stacked.last()` above). Reloading `regs[v]` reads an
        // unassigned slot — 0, the first parameter — so `n * fact(n-1)`
        // becomes `n * n`. Refuse; the planner parks these dests.
        MirInst::Call { dest, dest_hi, .. } if *dest == v || *dest_hi == Some(v) => {
            Err(LowerError::Refused(
                "stack-carried call result is not on TOS".into(),
            ))
        }
        _ => Err(LowerError::Refused("dense convoy value".into())),
    }
}

struct EmitStackBinArgs<'args> {
    out: &'args mut Vec<IlOp>,
    stacked: &'args mut Vec<ValueId>,
    op: MirBinOp,
    ty: MirTy,
    lhs: ValueId,
    rhs: ValueId,
    dest: ValueId,
    func: &'args MirFunc,
    plan: &'args ConvoyPlan,
    regs: &'args [u8],
    pool: &'args mut Vec<u64>,
    loc: DebugLoc,
}

fn emit_stack_bin(args: EmitStackBinArgs<'_>) -> Result<(), LowerError> {
    let EmitStackBinArgs {
        out,
        stacked,
        op,
        ty,
        lhs,
        rhs,
        dest,
        func,
        plan,
        regs,
        pool,
        loc,
    } = args;

    let stack_op = stack_bin_op(op, ty)?;
    if stacked.len() >= 2
        && stacked[stacked.len() - 2] == lhs
        && stacked[stacked.len() - 1] == rhs
    {
        out.push(IlOp::Bin {
            op: stack_op,
            loc,
        });
        stacked.pop();
        stacked.pop();
        stacked.push(dest);
        return Ok(());
    }
    if plan.needs_slot(lhs)
        && let Some(imm) = tree_i16(func, plan, rhs)
    {
        out.push(IlOp::BinSlotImm {
            op: stack_op as u8,
            slot: regs[lhs.index()],
            imm,
            loc,
        });
        stacked.push(dest);
        return Ok(());
    }
    emit_stack_value(EmitStackValueArgs {
        out,
        stacked,
        v: lhs,
        func,
        plan,
        regs,
        pool,
        loc,
    })?;
    emit_stack_value(EmitStackValueArgs {
        out,
        stacked,
        v: rhs,
        func,
        plan,
        regs,
        pool,
        loc,
    })?;
    out.push(IlOp::Bin {
        op: stack_op,
        loc,
    });
    stacked.pop();
    stacked.pop();
    stacked.push(dest);
    Ok(())
}

/// `v` as an `i16` immediate. Every `Const` qualifies here: the convoy
/// plan has no inline-tree set to gate on (LIR's `tree_i16` does).
fn tree_i16(func: &MirFunc, plan: &ConvoyPlan, v: ValueId) -> Option<i16> {
    const_i16(func, plan.def[v.index()])
}

fn emit_dense_push(
    out: &mut Vec<IlOp>,
    args: &[ValueId],
    regs: &[u8],
    scratch: u8,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    if args.is_empty() {
        return Ok(());
    }
    let arity = u8::try_from(args.len()).map_err(|_| LowerError::Refused("DensePush arity".into()))?;
    let slots: Vec<u8> = args.iter().map(|a| regs[a.index()]).collect();
    let base = gather_base(out, &slots, scratch, loc)?;
    out.push(IlOp::byte(
        Byte::new(Instruction::DensePush).with_dense_move(arity, base),
    ));
    Ok(())
}

/// `common::WORD_*` of a heap word of this MIR type. Not for an
/// [`MirFunc::opaque_words`] body: a type variable's word is typed `I64`
/// there and may be an object (#1955), so its kinds stay unknown (0).
fn mir_word_kind(ty: MirTy) -> u8 {
    match ty {
        MirTy::I64 | MirTy::F64 | MirTy::Bool => common::WORD_SCALAR,
        MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes => common::WORD_POINTER,
        MirTy::Value | MirTy::Bottom => common::WORD_UNKNOWN,
    }
}

fn dense_make_kind(kind: MirAllocKind) -> Result<Option<u8>, LowerError> {
    match kind {
        MirAllocKind::Array => Ok(Some(dense::MAKE_ARRAY)),
        MirAllocKind::Tuple => Ok(Some(dense::MAKE_TUPLE)),
        MirAllocKind::Enum { tag } => {
            let packed = u32::from(dense::MAKE_ENUM)
                .checked_add(tag)
                .ok_or_else(|| LowerError::Refused("DenseMake enum tag".into()))?;
            if packed > 255 {
                return Ok(None);
            }
            Ok(Some(packed as u8))
        }
        MirAllocKind::Object { .. } | MirAllocKind::DropEnum { .. } => Ok(None),
        MirAllocKind::Coro { .. }
        | MirAllocKind::Boxed { .. }
        | MirAllocKind::Fn { .. }
        | MirAllocKind::PolyFn { .. }
        | MirAllocKind::PolyFnCapture { .. } => Ok(None),
    }
}

fn dense_make_object(
    kind: MirAllocKind,
    dest: u8,
    loc: DebugLoc,
) -> Result<Option<IlOp>, LowerError> {
    let MirAllocKind::Object { type_id, nfields } = kind else {
        return Ok(None);
    };
    let nfields =
        u8::try_from(nfields).map_err(|_| LowerError::Refused("DenseMakeObject nfields".into()))?;
    let type_id =
        u16::try_from(type_id).map_err(|_| LowerError::Refused("DenseMakeObject type_id".into()))?;
    Ok(Some(IlOp::from_plain_byte(
        Byte::new(Instruction::DenseMakeObject)
            .with_operand_u32(dense::pack_make_object(dest, nfields, type_id)),
        loc,
    )))
}

fn emit_dense_field_load(
    out: &mut Vec<IlOp>,
    dest: u8,
    object: u8,
    index: u32,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    let c = field_dense_c(index)?;
    out.push(IlOp::from_plain_byte(
        Byte::new(Instruction::DenseFieldLoad).with_dense_abc(0, dest, object, c),
        loc,
    ));
    Ok(())
}

fn emit_dense_field_store(
    out: &mut Vec<IlOp>,
    dest: u8,
    object: u8,
    index: u32,
    loc: DebugLoc,
) -> Result<(), LowerError> {
    let c = field_dense_c(index)?;
    out.push(IlOp::from_plain_byte(
        Byte::new(Instruction::DenseFieldStore).with_dense_abc(0, dest, object, c),
        loc,
    ));
    Ok(())
}

/// A dense field op's slot operand: one that fits a byte. The compiler
/// emits only the slot form (no `FIELD_NAMED`).
fn field_dense_c(index: u32) -> Result<u8, LowerError> {
    u8::try_from(index).map_err(|_| LowerError::Refused("dense field index".into()))
}

/// A finalized enum made for its effect alone (`Conn::Open(fd);`): popped
/// as HIR emission pops it, not kept in a slot where it would stay
/// reachable, and its `drop` wait, until the slot is reused.
pub(super) fn dead_drop_alloc(func: &MirFunc, kind: MirAllocKind, dest: ValueId) -> bool {
    if !matches!(kind, MirAllocKind::DropEnum { .. }) {
        return false;
    }
    let mut used = false;
    for block in &func.blocks {
        used |= block.insts.iter().any(|i| i.operands().contains(&dest));
        if let Some(term) = &block.term {
            term.clone().rewrite_values(|v| {
                used |= v == dest;
                v
            });
        }
    }
    !used
}

/// Reconstruct fuse-IL alloc from SSA (`MakeArray` / `MakeTuple` / `MakeEnum` / `InitTyped`):
/// the ops after the elements are pushed. `kinds` is the `Alloc`'s recorded
/// construction kinds.
pub(super) fn il_for_alloc(
    kind: MirAllocKind,
    arity: u32,
    kinds: u8,
    loc: DebugLoc,
) -> Result<Vec<IlOp>, LowerError> {
    match kind {
        // The captures and filled parameters, then `Const mask`, `CodePtr`
        // and `MakeFn`, as HIR emission builds a function value.
        MirAllocKind::Fn { target, operand, mask } => Ok(vec![
            IlOp::Const { imm: mask as i32, loc },
            IlOp::Entry {
                kind: crate::il::EntryKind::CodePtr,
                arity: 0,
                target,
                loc,
                ret_words: 1,
            },
            IlOp::from_plain_byte(Byte::new(Instruction::MakeFn).with_operand_u32(operand), loc),
        ]),
        // `TagEnumType` keeps the value on the stack and stamps its type.
        MirAllocKind::DropEnum { tag, type_id } => Ok(vec![
            il_for_alloc_op(MirAllocKind::Enum { tag }, arity, kinds, loc)?,
            IlOp::from_plain_byte(Byte::new(Instruction::TagEnumType).with_operand_u32(type_id), loc),
        ]),
        // The dictionaries, then `CodePtr` and `MakePolyFnCapture`, as
        // HIR emission builds a constrained generic function's value.
        MirAllocKind::PolyFnCapture { target, params } => Ok(vec![
            code_ptr_op(target, loc),
            IlOp::from_plain_byte(Byte::new(Instruction::MakePolyFnCapture).with_operand_u32(arity | (params << 8)), loc),
        ]),
        MirAllocKind::PolyFn { target } => Ok(vec![IlOp::Entry {
            kind: crate::il::EntryKind::MakePolyFn,
            arity: 0,
            target,
            loc,
            ret_words: 1,
        }]),
        _ => il_for_alloc_op(kind, arity, kinds, loc).map(|op| vec![op]),
    }
}

/// `CallIndirect`'s operand: the value arguments, and the instance
/// dictionaries after them in the high half.
pub(super) fn call_indirect_operand(args: usize, dicts: u32) -> u32 {
    (args as u32 - dicts) | (dicts << 16)
}

/// `CodePtr` of the function at `target`.
pub(super) fn code_ptr_op(target: Label, loc: DebugLoc) -> IlOp {
    IlOp::Entry {
        kind: crate::il::EntryKind::CodePtr,
        arity: 0,
        target,
        loc,
        ret_words: 1,
    }
}

fn il_for_alloc_op(kind: MirAllocKind, arity: u32, kinds: u8, loc: DebugLoc) -> Result<IlOp, LowerError> {
    match kind {
        MirAllocKind::Array => Ok(IlOp::MakeArray { elem_kind: kinds, arity, loc }),
        MirAllocKind::Tuple => Ok(IlOp::MakeTuple { kinds, arity, loc }),
        MirAllocKind::Enum { tag } => {
            let tag = u16::try_from(tag).map_err(|_| LowerError::Refused("enum tag".into()))?;
            let arity =
                u16::try_from(arity).map_err(|_| LowerError::Refused("enum arity".into()))?;
            Ok(IlOp::MakeEnum { kinds, tag, arity, loc })
        }
        MirAllocKind::DropEnum { .. } => Err(LowerError::Refused("drop enum op".into())),
        MirAllocKind::Object { type_id, nfields } => Ok(IlOp::byte(
            Byte::new(Instruction::InitTyped)
                .with_operand_u32(common::pack_init_typed(type_id, nfields)),
        )),
        MirAllocKind::Coro { target } => Ok(IlOp::Entry {
            kind: crate::il::EntryKind::MakeCoro,
            arity,
            target,
            loc,
            ret_words: 1,
        }),
        MirAllocKind::Boxed { tag } => Ok(IlOp::BoxValue { tag, loc }),
        MirAllocKind::Fn { .. } | MirAllocKind::PolyFn { .. } | MirAllocKind::PolyFnCapture { .. } => Err(LowerError::Refused("function value alloc".into())),
    }
}

fn object_make_dest_reg(
    kind: MirAllocKind,
    alloc: ValueId,
    func: &MirFunc,
    regs: &[u8],
) -> Option<u8> {
    let MirAllocKind::Object { .. } = kind else {
        return None;
    };
    let live = paired_barrier_for_alloc(func, alloc).unwrap_or(alloc);
    Some(regs[live.index()])
}

/// Barrier dest that is the live InitTyped identity (users load this, not Alloc).
fn paired_barrier_for_alloc(func: &MirFunc, alloc: ValueId) -> Option<ValueId> {
    for block in &func.blocks {
        let mut pending = false;
        for inst in &block.insts {
            match inst {
                MirInst::Alloc { dest, .. } if *dest == alloc => pending = true,
                MirInst::GcBarrier { dest, .. } if pending => return Some(*dest),
                MirInst::Alloc { .. }
                | MirInst::ArrayPush { .. }
                | MirInst::Format { .. }
                | MirInst::Stringify { .. } => pending = false,
                _ if pending => pending = false,
                _ => {}
            }
        }
    }
    None
}

/// Alloc dest paired with a `GcBarrier` dest in the same block.
pub(super) fn paired_alloc_dest(func: &MirFunc, barrier: ValueId) -> Option<ValueId> {
    for block in &func.blocks {
        let mut pending = None;
        for inst in &block.insts {
            match inst {
                MirInst::Alloc { dest, .. }
                | MirInst::ArrayPush { dest, .. }
                | MirInst::Format { dest, .. }
                | MirInst::Stringify { dest, .. } => {
                    pending = Some(*dest)
                }
                MirInst::GcBarrier { dest, .. } if *dest == barrier => return pending,
                _ => pending = None,
            }
        }
    }
    None
}

/// [`paired_alloc_dest`] of every barrier, by its dest, in one pass: a
/// body asking once per barrier scanned the whole body each time (#1502).
pub(super) fn paired_alloc_dests(func: &MirFunc) -> HashMap<ValueId, ValueId> {
    let mut out = HashMap::new();
    for block in &func.blocks {
        let mut pending = None;
        for inst in &block.insts {
            match inst {
                MirInst::Alloc { dest, .. }
                | MirInst::ArrayPush { dest, .. }
                | MirInst::Format { dest, .. }
                | MirInst::Stringify { dest, .. } => pending = Some(*dest),
                MirInst::GcBarrier { dest, .. } => {
                    // The first barrier of a dest is the one the scan finds.
                    if let Some(obj) = pending {
                        out.entry(*dest).or_insert(obj);
                    }
                    pending = None;
                }
                _ => pending = None,
            }
        }
    }
    out
}

/// A `ByteScan`'s `VScan` body: each stream loaded once into the top
/// vector registers (stream `s` in `v7 - s`), then byte-lane `VBin`s, each
/// tree node into a register counted up from `v0`, so the mask ends in
/// `v0`. A `VLoad`'s index slot holds the stream's offset from the scan
/// position.
fn byte_scan_body(
    expr: &super::inst::LaneExpr,
    streams: &[(ValueId, ValueId)],
    scalars: &[ValueId],
    regs: &[u8],
) -> Option<Vec<Byte>> {
    use super::inst::{LaneExpr, LaneVal};
    use common::simd;

    struct Ctx<'a> {
        out: Vec<Byte>,
        next: u8,
        /// The lowest stream register: nodes stay below it.
        top: u8,
        scalars: &'a [ValueId],
        regs: &'a [u8],
    }

    impl Ctx<'_> {
        fn reg(&mut self) -> Option<u8> {
            let r = self.next;
            if r >= self.top {
                return None;
            }
            self.next += 1;
            Some(r)
        }

        fn vbin(&mut self, kind: u8, dest: u8, a: u8, b: u8) {
            self.out.push(Byte::new(Instruction::VBin).with_dense_abc(kind, dest, a, b));
        }

        fn stream(&self, s: u8) -> Option<u8> {
            let r = (simd::NREGS - 1).checked_sub(usize::from(s))? as u8;
            (r >= self.top).then_some(r)
        }

        fn eval(&mut self, e: &LaneExpr) -> Option<u8> {
            let pred = |op: MirCmpOp| match op {
                MirCmpOp::Lt => dense::CMP_LT,
                MirCmpOp::Le => dense::CMP_LE,
                MirCmpOp::Gt => dense::CMP_GT,
                MirCmpOp::Ge => dense::CMP_GE,
                MirCmpOp::Eq => dense::CMP_EQ,
                MirCmpOp::Ne => dense::CMP_NE,
            };
            let base = self.next;
            let r = match e {
                // `x == x` / `x != x` on a loaded lane.
                LaneExpr::Const(b) => {
                    let (r, x) = (self.reg()?, self.stream(0)?);
                    let k = if *b { dense::CMP_EQ } else { dense::CMP_NE };
                    self.vbin(simd::B_CMP + k, r, x, x);
                    r
                }
                LaneExpr::Cmp { op, lhs, rhs } => match (*lhs, *rhs) {
                    (LaneVal::Stream(a), LaneVal::Stream(b)) => {
                        let (r, x, y) = (self.reg()?, self.stream(a)?, self.stream(b)?);
                        self.vbin(simd::B_CMP + pred(*op), r, x, y);
                        r
                    }
                    (LaneVal::Stream(a), LaneVal::Scalar(k)) => {
                        let (r, x) = (self.reg()?, self.stream(a)?);
                        let slot = self.regs[self.scalars.get(usize::from(k))?.index()];
                        self.vbin(simd::BS_CMP + pred(*op), r, slot, x);
                        r
                    }
                    (LaneVal::Scalar(_), LaneVal::Stream(_)) => {
                        let flipped = match op {
                            MirCmpOp::Lt => MirCmpOp::Gt,
                            MirCmpOp::Le => MirCmpOp::Ge,
                            MirCmpOp::Gt => MirCmpOp::Lt,
                            MirCmpOp::Ge => MirCmpOp::Le,
                            o => *o,
                        };
                        return self.eval(&LaneExpr::Cmp { op: flipped, lhs: *rhs, rhs: *lhs });
                    }
                    (LaneVal::Scalar(_), LaneVal::Scalar(_)) => return None,
                },
                LaneExpr::And(a, b) | LaneExpr::Or(a, b) => {
                    let x = self.eval(a)?;
                    let y = self.eval(b)?;
                    let kind = if matches!(e, LaneExpr::And(..)) { simd::M_AND } else { simd::M_OR };
                    self.vbin(kind, x, x, y);
                    x
                }
                LaneExpr::Not(a) => {
                    let x = self.eval(a)?;
                    self.vbin(simd::M_NOT, x, x, x);
                    x
                }
            };
            // Registers past the result are free again.
            debug_assert_eq!(r, base);
            self.next = base + 1;
            Some(r)
        }
    }

    let top = simd::NREGS.checked_sub(streams.len())? as u8;
    let mut cx = Ctx { out: Vec::new(), next: 0, top, scalars, regs };
    for (s, &(arr, off)) in streams.iter().enumerate() {
        let r = cx.stream(s as u8)?;
        cx.out.push(Byte::new(Instruction::VLoad).with_dense_abc(
            simd::TY_U8,
            r,
            regs[arr.index()],
            regs[off.index()],
        ));
    }
    cx.eval(expr)?;
    (cx.out.len() <= simd::SCAN_MAX_BODY).then_some(cx.out)
}
