//! IL optimization passes unlocked by symbolic labels.
//!
//! **Driver (D2).** Production opts run from a static table in [`driver`]
//! (order matches D1 README). Each [`driver::Pass`] returns a
//! [`stats::PassDelta`]; `collect_stats` records that delta (`PassKind` lives
//! on the table row, not a match in the driver loop).
//! [`super::IlModule::optimize_and_flatten`] still defers
//! `multi_op_join_convoy`, `invert_guard_branch`, `seek_back_edge`,
//! `slot_promote_tell`, and `ssa_gvn` around per-body `cfg_gvn` — those are not folded into
//! the OptLevel table. Fuse-select stays in `lower_optimized`.
//!
//! Per-pass contracts (input, output, refusals, solo tests): see `README.md` in this directory.

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
    /// Delay `STORE t` across slot-addressed ops so `LOAD t; STORE s` pops TOS.
    pub tos_carry: bool,
    /// Operand-order canon (`Const;Load` → `Load;Const`, load/load slot order).
    pub canon: bool,
    /// Spill `CastIntToFloat` ahead of float-arith → STORE windows.
    pub cast_spill: bool,
    /// Algebraic / strength peeps (x+0, x*1, cmp fold, …) when SP Known.
    pub algebraic: bool,
    /// Local InstCombine / peephole (const-cond branches, pair-match identity).
    pub instcombine: bool,
    /// Dominated bounds / niche / tag checks (Index→Unchecked, known-tag EQ).
    pub dom_check: bool,
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
    /// that makes the header `Known` and exposes in-loop self-stores.
    /// Aggressive-only: Seek poisons operand-height at the latch (cursor), not
    /// to protect fused opcodes.
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
    /// Heuristic branch layout (COI-128).
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

// Default is `OptLevel::Standard.options()` (derived from the driver table).

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
#[cfg(test)]
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
#[cfg(test)]
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
    driver::run_once(ops, opts, entry_sp, pool, next_label);
}

/// Run [`optimize`] on each [`super::IlFunc`] emitting span; leave prologue and
/// inter-function glue untouched. Falls back to whole-buffer opts when `funcs`
/// is empty (unit tests / buffers without `record_func`).
///
/// Thin flat-buffer wrapper over [`super::IlModule::optimize_and_flatten`].
/// Production lower uses [`super::CodeBuf::lower_in_place`] /
/// [`super::lower::lower_module_inner`] on an owning module; this
/// stays for unit tests that mutate a bare `Vec<IlOp>`.
///
/// Whole-buffer [`multi_op_join_convoy`] is required: scoped multi_op can treat
/// JMPF/fall-through diamonds as SP-known and mis-sink (e.g. `examples/fib.hy`).
#[cfg(test)]
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
    let (optimized, _, _) = module.optimize_and_flatten(opts, pool);
    *ops = optimized;
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
mod driver;
mod opt_level;
mod stats;
pub(crate) use branch_opt::{max_code_label, remap_label_space};
pub use opt_level::OptLevel;
pub(crate) use stats::note_function_inlined;
pub use stats::{OptStats, begin_opt_stats, last_opt_stats};

mod cfg;
mod convoy;
mod dce;
mod instcombine;
mod dom_check;
mod escape_analysis;
mod invariant_store_elim;
mod loop_unroll;
mod slot_promote;
mod tos_carry;

pub(crate) use cfg::invert_branch_over_jump as invert_guard_branch;
pub(crate) use convoy::multi_op_join_convoy;
pub(crate) use slot_promote::{seek_normalize_back_edges, slot_promote_at};

#[cfg(test)]
#[path = "mod.tests.rs"]
mod iterative_opt_tests;
