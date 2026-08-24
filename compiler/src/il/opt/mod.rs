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
    /// When a PGO profile is loaded, unroll hotter loops first (COI-190).
    pub pgo_prioritize_hot_loops: bool,
    /// Sink or drop loop stores of an invariant value that is not read in the loop.
    pub invariant_store_elim: bool,
    /// SSA-style global CSE of pure binops whose result already lives in a slot.
    pub ssa_gvn: bool,
    /// Scalarize non-escaping `MakeArray` into consecutive frame slots (COI-126).
    pub escape_analysis: bool,
    /// Heuristic / profile-guided branch layout (COI-128).
    /// Default **on**: invert only Known-SP terminating then-arms, and mint
    /// labels from a module-wide watermark so ids cannot collide across funcs.
    pub branch_optimization: bool,
    /// Sink jump-only terminating blocks to the end (COI-129). Fall-through
    /// chains stay adjacent; branch labels are not rewritten.
    pub block_reordering: bool,
    /// Re-run the pass pipeline until a round is a no-op, or
    /// [`Self::max_optimization_iterations`] (COI-130). Default **off**.
    pub iterative_optimization: bool,
    /// Cap on full pipeline rounds when [`Self::iterative_optimization`] is on.
    /// Clamped to `1..=10` at run time.
    pub max_optimization_iterations: usize,
    /// Record per-pass counters into [`stats::OptStats`] (COI-131). Default **off**.
    pub collect_stats: bool,
    /// Pure user `fn` names + entry labels for COI-99 length-proof barriers.
    pub pure_call_ctx: Option<super::pure_call::PureCallCtx>,
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
            pgo_prioritize_hot_loops: true,
            invariant_store_elim: true,
            ssa_gvn: true,
            escape_analysis: true,
            branch_optimization: true,
            block_reordering: true,
            iterative_optimization: false,
            max_optimization_iterations: 10,
            collect_stats: false,
            pure_call_ctx: None,
        }
    }
}

/// One pipeline round: whether the op buffer changed, and its length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PassStats {
    pub changed: bool,
    pub ops_before: usize,
    pub ops_after: usize,
}

/// Result of [`optimize_iteratively`]: round count and whether a no-op round
/// was observed before the iteration cap.
///
/// Per-pass counters (COI-176) live on [`OptStats`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OptimizationStats {
    pub iterations: usize,
    pub converged: bool,
    pub hit_iteration_limit: bool,
    pub passes: Vec<PassStats>,
}

/// Run the current pipeline once. Ignores [`OptimizeOptions::iterative_optimization`].
pub fn run_optimization_pass(
    ops: &mut Vec<IlOp>,
    opts: &OptimizeOptions,
    pool: &mut Vec<u64>,
) -> PassStats {
    let mut next = branch_opt::next_fresh_label(ops);
    run_optimization_pass_at(ops, opts, 0, pool, &mut next)
}

fn run_optimization_pass_at(
    ops: &mut Vec<IlOp>,
    opts: &OptimizeOptions,
    entry_sp: i32,
    pool: &mut Vec<u64>,
    next_label: &mut u32,
) -> PassStats {
    let before = ops.clone();
    optimize_once_at(ops, opts, entry_sp, pool, next_label);
    PassStats {
        changed: *ops != before,
        ops_before: before.len(),
        ops_after: ops.len(),
    }
}

/// Repeat [`run_optimization_pass`] until a round is a no-op or `max_iterations`
/// (clamped to `1..=10`) is reached.
pub fn optimize_iteratively(
    ops: &mut Vec<IlOp>,
    opts: &OptimizeOptions,
    pool: &mut Vec<u64>,
    max_iterations: usize,
) -> OptimizationStats {
    let mut next = branch_opt::next_fresh_label(ops);
    optimize_iteratively_at(ops, opts, 0, pool, max_iterations, &mut next)
}

fn optimize_iteratively_at(
    ops: &mut Vec<IlOp>,
    opts: &OptimizeOptions,
    entry_sp: i32,
    pool: &mut Vec<u64>,
    max_iterations: usize,
    next_label: &mut u32,
) -> OptimizationStats {
    let cap = max_iterations.clamp(1, 10);
    let mut passes = Vec::new();
    for i in 1..=cap {
        let stats = run_optimization_pass_at(ops, opts, entry_sp, pool, next_label);
        let changed = stats.changed;
        passes.push(stats);
        if !changed {
            return OptimizationStats {
                iterations: i,
                converged: true,
                hit_iteration_limit: false,
                passes,
            };
        }
    }
    OptimizationStats {
        iterations: cap,
        converged: false,
        hit_iteration_limit: true,
        passes,
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
    let mut next = branch_opt::next_fresh_label(ops);
    optimize_at_with_labels(ops, opts, entry_sp, pool, &mut next);
}

pub(crate) fn optimize_at_with_labels(
    ops: &mut Vec<IlOp>,
    opts: &OptimizeOptions,
    entry_sp: i32,
    pool: &mut Vec<u64>,
    next_label: &mut u32,
) {
    if opts.iterative_optimization {
        let round = optimize_iteratively_at(
            ops,
            opts,
            entry_sp,
            pool,
            opts.max_optimization_iterations,
            next_label,
        );
        if opts.collect_stats {
            stats::set_iterations(round.iterations);
        }
        return;
    }
    if opts.collect_stats {
        stats::set_iterations(1);
    }
    optimize_once_at(ops, opts, entry_sp, pool, next_label);
}

fn optimize_once_at(
    ops: &mut Vec<IlOp>,
    opts: &OptimizeOptions,
    entry_sp: i32,
    pool: &mut Vec<u64>,
    next_label: &mut u32,
) {
    let entry_tell = cleanup_once_at(ops, opts, entry_sp, pool);
    // Instrument compile: counters describe cleanup mid-IR only.
    if crate::profile::pgo_instrumenting() {
        return;
    }
    crate::profile::prepare_function_profile(ops);
    decision_once_at(ops, opts, entry_sp, entry_tell, next_label);
}

/// Profile-agnostic cleanup: peeps that normalize shape without layout/heat.
fn cleanup_once_at(
    ops: &mut Vec<IlOp>,
    opts: &OptimizeOptions,
    entry_sp: i32,
    pool: &mut Vec<u64>,
) -> u32 {
    let collect = opts.collect_stats;
    let g = stats::PassKind::Generic;
    if opts.jump_thread {
        stats::run_named_pass(ops, collect, "jump_thread", g, |ops| {
            jump_thread(ops);
            0
        });
    }
    if opts.dead_block {
        stats::run_named_pass(ops, collect, "dead_block", g, |ops| {
            eliminate_dead_blocks(ops);
            0
        });
    }
    if opts.stack_dce {
        stats::run_named_pass(ops, collect, "stack_dce", g, |ops| {
            stack_dce(ops);
            0
        });
    }
    if opts.mem_fwd {
        stats::run_named_pass(ops, collect, "mem_fwd", g, |ops| {
            mem_fwd(ops, entry_sp);
            0
        });
    }
    let entry_tell = entry_sp.max(0) as u32;
    if opts.copy_prop {
        stats::run_named_pass(ops, collect, "copy_prop", g, |ops| {
            copy_prop(ops, entry_tell);
            0
        });
    }
    if opts.mem_fwd {
        stats::run_named_pass(ops, collect, "dead_store", g, |ops| {
            dead_store_at(ops, entry_tell);
            0
        });
    }
    if opts.canon {
        stats::run_named_pass(ops, collect, "canon", g, |ops| {
            super::canon::canonicalize_operand_order(ops, pool);
            0
        });
    }
    if opts.algebraic {
        stats::run_named_pass(ops, collect, "algebraic", g, |ops| {
            super::algebraic::algebraic_simplify(ops, pool);
            0
        });
    }
    if opts.cast_spill {
        stats::run_named_pass(ops, collect, "cast_spill", g, |ops| {
            super::cast_spill::spill_cast_before_float_chain(ops);
            0
        });
    }
    entry_tell
}

/// Profile-sensitive mid opts; branch layout and block reorder stay last.
fn decision_once_at(
    ops: &mut Vec<IlOp>,
    opts: &OptimizeOptions,
    entry_sp: i32,
    entry_tell: u32,
    next_label: &mut u32,
) {
    let collect = opts.collect_stats;
    let g = stats::PassKind::Generic;
    if opts.licm {
        let _ = entry_sp;
        stats::run_named_pass(ops, collect, "licm", g, |ops| {
            super::licm::set_pgo_prioritize_hot_licm(opts.pgo_prioritize_hot_loops);
            super::licm::licm(ops);
            0
        });
    }
    if opts.loop_bounds {
        stats::run_named_pass(ops, collect, "loop_bounds", g, |ops| {
            super::bounds::loop_bounds(ops);
            0
        });
    }
    if opts.loop_unroll {
        stats::run_named_pass(
            ops,
            collect,
            "loop_unroll",
            stats::PassKind::Unroll,
            |ops| {
                loop_unroll::unroll_loops_pgo(
                    ops,
                    opts.loop_unroll_factor,
                    opts.pgo_prioritize_hot_loops,
                )
            },
        );
    }
    if opts.invariant_store_elim {
        stats::run_named_pass(ops, collect, "invariant_store_elim", g, |ops| {
            invariant_store_elim::eliminate_invariant_stores(ops, entry_sp);
            0
        });
    }
    if opts.ssa_gvn {
        stats::run_named_pass(ops, collect, "ssa_gvn", g, |ops| {
            super::gvn_ssa::ssa_gvn(ops);
            0
        });
    }
    // After GVN so scalarized loads can CSE; before slot_promote so new
    // element slots are eligible for promotion.
    if opts.escape_analysis {
        stats::run_named_pass(ops, collect, "escape_analysis", g, |ops| {
            escape_analysis::escape_analysis_pgo(ops, opts.pgo_prioritize_hot_loops);
            0
        });
    }
    // After LICM so hoisted `LOAD temp; STORE local` copies become aliases.
    if opts.slot_promote {
        stats::run_named_pass(ops, collect, "slot_promote", g, |ops| {
            slot_promote(ops, entry_tell);
            dead_store_at(ops, entry_tell);
            0
        });
    }
    if opts.clone_shared_return {
        stats::run_named_pass(ops, collect, "clone_shared_return", g, |ops| {
            clone_shared_return(ops);
            0
        });
    }
    if opts.return_convoy {
        stats::run_named_pass(ops, collect, "return_convoy", g, |ops| {
            return_convoy(ops);
            0
        });
    }
    if opts.bin_join_convoy {
        stats::run_named_pass(ops, collect, "bin_join_convoy", g, |ops| {
            bin_join_convoy(ops);
            0
        });
    }
    if opts.multi_op_join_convoy {
        stats::run_named_pass(ops, collect, "multi_op_join_convoy", g, |ops| {
            multi_op_join_convoy(ops);
            0
        });
    }
    // Last among shape rewrites: convoy passes match JMP-to-join shapes.
    if opts.invert_guard_branch {
        stats::run_named_pass(ops, collect, "invert_guard_branch", g, |ops| {
            invert_branch_over_jump(ops);
            0
        });
    }
    if opts.branch_optimization {
        stats::run_named_pass(
            ops,
            collect,
            "branch_optimization",
            stats::PassKind::Branch,
            |ops| {
                let profile = crate::profile::current_profile();
                let bp = profile
                    .as_ref()
                    .map(|p| crate::profile::branch_profile(ops, p));
                branch_opt::optimize_branches_at(ops, bp.as_ref(), entry_sp, next_label)
            },
        );
    }
    if opts.block_reordering {
        stats::run_named_pass(
            ops,
            collect,
            "block_reordering",
            stats::PassKind::BlockOrder,
            |ops| {
                let profile = crate::profile::current_profile();
                let bp = profile
                    .as_ref()
                    .map(|p| crate::profile::branch_profile(ops, p));
                block_order::reorder_basic_blocks(ops, bp.as_ref())
            },
        );
    }
    // After every slot-tracking pass: promotion leaves a slot defined only by
    // the push that lands on it, which earlier passes would not see.
    // Seek-normalize first so loop headers join at a Known cursor.
    if opts.seek_back_edge {
        stats::run_named_pass(ops, collect, "seek_back_edge", g, |ops| {
            seek_normalize_back_edges(ops, entry_tell);
            0
        });
    }
    if opts.slot_promote_tell {
        stats::run_named_pass(ops, collect, "slot_promote_tell", g, |ops| {
            slot_promote_at(ops, entry_tell);
            0
        });
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
mod stats;
pub use opt_level::OptLevel;
pub use stats::{OptStats, begin_opt_stats, last_opt_stats};
pub(crate) use stats::note_function_inlined;
#[allow(unused_imports)]
pub use branch_opt::{BranchProfile, optimize_branches};
pub(crate) use branch_opt::{max_code_label, remap_label_space};
pub(crate) use block_order::reorder_basic_blocks;

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

#[cfg(test)]
#[path = "mod.tests.rs"]
mod iterative_opt_tests;
