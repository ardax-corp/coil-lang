//! IL optimization passes unlocked by symbolic labels.

use super::op::IlOp;

/// Options for [`optimize`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OptimizeOptions {
    /// Collapse `JMP L` where `L` begins with `JMP L2` into `JMP L2`.
    pub jump_thread: bool,
    /// Remove unreachable ops after unconditional JMP / RETURN until a label.
    pub dead_block: bool,
    /// Drop redundant `DUPLICATE; POP` and `LOAD s; StorePop s`.
    pub stack_dce: bool,
    /// `StorePop s; Load s` → `Dup; StorePop s`; dead-store elimination.
    pub mem_fwd: bool,
    /// Forward pure producer copies through cursor-safe straight-line regions.
    pub copy_prop: bool,
    /// Promote slots to virtual values (straight-line + same-def joins).
    pub slot_promote: bool,
    /// Operand-order canon (`Const;Load` → `Load;Const`, load/load slot order).
    pub canon: bool,
    /// Spill `CastIntToFloat` that blocks FloatChainStore fuse windows.
    pub cast_spill: bool,
    /// Algebraic / strength peeps (x+0, x*1, cmp fold, …) when SP Known.
    pub algebraic: bool,
    /// Hoist invariant Const/Load out of Known-SP natural loops.
    pub licm: bool,
    /// Counted-loop ArrayLen hoist + Index/StoreIndex bounds proofs.
    pub loop_bounds: bool,
    /// Sink identical `LOAD`/`CONST` producers into a join `RETURN` and fuse.
    pub return_convoy: bool,
    /// Clone plain `RETURN` onto jump-only preds of mixed return joins.
    pub clone_shared_return: bool,
    /// Sink identical binop / BinSlot* tails into a return-label cluster.
    pub bin_join_convoy: bool,
    /// Sink identical multi-op suffixes (len 2..=4) at return / non-return joins.
    pub multi_op_join_convoy: bool,
    /// `JMPF A; JMP B; A:` → `JMPT B` for non-fusable guard conditions.
    pub invert_guard_branch: bool,
    /// Drop `LOAD`/`STORE` the shared cursor proves redundant, promoting the
    /// slot out of the frame. Runs last, after every slot-tracking pass.
    pub slot_promote_tell: bool,
    /// `Seek` the latch of a natural loop back to the forward-edge cursor when
    /// that makes the header `Known` and exposes in-loop self-stores (COI-97).
    /// Off: innermost mandelbrot has no such self-stores; outer-loop Seek
    /// splits FloatChainStore fuse windows.
    pub seek_back_edge: bool,
    /// Full-unroll counted natural loops with a known trip count ≤ 8.
    pub loop_unroll: bool,
    /// Cap on trips fully unrolled (clamped to 8). Loops with more trips stay rolled.
    pub loop_unroll_factor: usize,
    /// Sink or drop loop stores of an invariant value that is not read in the loop.
    pub invariant_store_elim: bool,
    /// SSA-style global CSE of pure binops whose result already lives in a slot.
    pub ssa_gvn: bool,
    /// Scalarize non-escaping `MakeArray` into consecutive frame slots (COI-126).
    pub escape_analysis: bool,
    /// Heuristic / profile-guided branch layout (COI-128).
    /// Default **off**: moving a then-arm is not yet SP-safe on fused
    /// production IL (`examples/fib.hy`). Tests call the pass directly.
    pub branch_optimization: bool,
    /// Sink jump-only terminating blocks to the end (COI-129). Fall-through
    /// chains stay adjacent; branch labels are not rewritten.
    pub block_reordering: bool,
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        Self {
            jump_thread: true,
            dead_block: true,
            stack_dce: true,
            mem_fwd: true,
            copy_prop: true,
            slot_promote: true,
            canon: true,
            // On: spill casts that block float chains; fuse stage0 accepts
            // LOAD;CONST so the spilled shape becomes FloatChainStore.
            cast_spill: true,
            algebraic: true,
            licm: true,
            loop_bounds: true,
            return_convoy: true,
            clone_shared_return: true,
            bin_join_convoy: true,
            multi_op_join_convoy: true,
            invert_guard_branch: true,
            slot_promote_tell: true,
            seek_back_edge: false,
            loop_unroll: true,
            loop_unroll_factor: 8,
            invariant_store_elim: true,
            ssa_gvn: true,
            escape_analysis: true,
            branch_optimization: false,
            block_reordering: true,
        }
    }
}

/// Run IL opts in place. Safe to call before [`super::lower`].
///
/// Pass the const pool when available so algebraic float peeps can read
/// `ConstPool` bits and push folded IEEE results; an empty vec disables those.
pub fn optimize(ops: &mut Vec<IlOp>, opts: &OptimizeOptions, pool: &mut Vec<u64>) {
    optimize_at(ops, opts, 0, pool);
}

/// Like [`optimize`], seeding SP analysis at `entry_sp` for the op buffer.
pub fn optimize_at(
    ops: &mut Vec<IlOp>,
    opts: &OptimizeOptions,
    entry_sp: i32,
    pool: &mut Vec<u64>,
) {
    if opts.jump_thread {
        jump_thread(ops);
    }
    if opts.dead_block {
        eliminate_dead_blocks(ops);
    }
    if opts.stack_dce {
        stack_dce(ops);
    }
    if opts.mem_fwd {
        mem_fwd(ops, entry_sp);
    }
    let entry_tell = entry_sp.max(0) as u32;
    if opts.copy_prop {
        copy_prop(ops, entry_tell);
    }
    if opts.mem_fwd {
        dead_store_at(ops, entry_tell);
    }
    if opts.canon {
        super::canon::canonicalize_operand_order(ops, pool);
    }
    if opts.algebraic {
        super::algebraic::algebraic_simplify(ops, pool);
    }
    if opts.cast_spill {
        super::cast_spill::spill_cast_before_float_chain(ops);
    }
    if opts.licm {
        // LICM still seeds at 0; entry_sp plumbing is mem_fwd-critical today.
        let _ = entry_sp;
        super::licm::licm(ops);
    }
    if opts.loop_bounds {
        super::bounds::loop_bounds(ops);
    }
    if opts.loop_unroll {
        loop_unroll::unroll_loops(ops, opts.loop_unroll_factor);
    }
    if opts.invariant_store_elim {
        invariant_store_elim::eliminate_invariant_stores(ops, entry_sp);
    }
    if opts.ssa_gvn {
        super::gvn_ssa::ssa_gvn(ops);
    }
    // After GVN so scalarized loads can CSE; before slot_promote so new
    // element slots are eligible for promotion.
    if opts.escape_analysis {
        escape_analysis::escape_analysis(ops);
    }
    // After LICM so hoisted `LOAD temp; STORE local` copies become aliases.
    if opts.slot_promote {
        slot_promote(ops, entry_tell);
        dead_store_at(ops, entry_tell);
    }
    if opts.clone_shared_return {
        clone_shared_return(ops);
    }
    if opts.return_convoy {
        return_convoy(ops);
    }
    if opts.bin_join_convoy {
        bin_join_convoy(ops);
    }
    if opts.multi_op_join_convoy {
        multi_op_join_convoy(ops);
    }
    // Last: the convoy passes match on JMP-to-join shapes this would remove.
    if opts.invert_guard_branch {
        invert_branch_over_jump(ops);
    }
    if opts.branch_optimization {
        branch_opt::optimize_branches(ops, None);
    }
    if opts.block_reordering {
        block_order::reorder_basic_blocks(ops, None);
    }
    // After every slot-tracking pass: promotion leaves a slot defined only by
    // the push that lands on it, which earlier passes would not see.
    // Seek-normalize first so loop headers join at a Known cursor.
    if opts.seek_back_edge {
        seek_normalize_back_edges(ops, entry_tell);
    }
    if opts.slot_promote_tell {
        slot_promote_at(ops, entry_tell);
    }
}

/// Run [`optimize`] on each [`super::IlFunc`] emitting span; leave prologue and
/// inter-function glue untouched. Falls back to whole-buffer opts when `funcs`
/// is empty (unit tests / buffers without `record_func`).
///
/// Thin flat-buffer wrapper over [`super::IlModule::optimize_and_flatten`].
/// Production lower uses [`super::lower_module`] on an owning module; this
/// stays for unit tests that mutate a bare `Vec<IlOp>`.
///
/// Whole-buffer [`multi_op_join_convoy`] is required: scoped multi_op can treat
/// JMPF/fall-through diamonds as SP-known and mis-sink (e.g. `examples/fib.hy`).
#[allow(dead_code)]
pub fn optimize_per_func(
    ops: &mut Vec<IlOp>,
    funcs: &[super::IlFunc],
    opts: &OptimizeOptions,
    pool: &mut Vec<u64>,
) {
    if funcs.is_empty() {
        optimize(ops, opts, pool);
        return;
    }

    let mut module = super::IlModule::from_flat(ops, funcs);
    *ops = module.optimize_and_flatten(opts, pool);
}

/// Map inclusive-exclusive emitting indices to a raw op range, including
/// leading labels bound at `emit_start`.
pub(crate) fn emitting_range_to_raw(
    ops: &[IlOp],
    emit_start: usize,
    emit_end: usize,
) -> (usize, usize) {
    let mut emitting = 0usize;
    let mut raw_start: Option<usize> = None;
    let mut raw_end: Option<usize> = None;
    for (i, op) in ops.iter().enumerate() {
        if emitting == emit_start && raw_start.is_none() {
            let mut s = i;
            while s > 0 && !ops[s - 1].emits_code() {
                s -= 1;
            }
            raw_start = Some(s);
        }
        if !op.emits_code() {
            continue;
        }
        emitting += 1;
        if emitting == emit_end {
            raw_end = Some(i + 1);
            break;
        }
    }
    (
        raw_start.unwrap_or(0),
        raw_end.unwrap_or_else(|| {
            // emit_end past buffer: take through end once start was found.
            if raw_start.is_some() { ops.len() } else { 0 }
        }),
    )
}

mod block_order;
mod branch_opt;
mod opt_level;
pub use opt_level::OptLevel;
#[allow(unused_imports)]
pub use branch_opt::{BranchProfile, optimize_branches};

mod cfg;
mod convoy;
mod dce;
mod escape_analysis;
mod invariant_store_elim;
mod loop_unroll;
mod slot_promote;

pub(crate) use cfg::invert_branch_over_jump as invert_guard_branch;
use cfg::{eliminate_dead_blocks, invert_branch_over_jump, jump_thread};
pub(crate) use convoy::multi_op_join_convoy;
use convoy::{bin_join_convoy, clone_shared_return, return_convoy};
use dce::{copy_prop, dead_store_at, mem_fwd, stack_dce};
use slot_promote::slot_promote;
pub(crate) use slot_promote::{seek_normalize_back_edges, slot_promote_at};
