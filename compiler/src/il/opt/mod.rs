//! IL optimization passes unlocked by symbolic labels.
//!
//! **Driver (D2).** Production opts run from a static table in [`driver`]
//! (order matches D1 README). Each [`driver::Pass`] returns a
//! [`stats::PassDelta`]; `collect_stats` records that delta (`PassKind` lives
//! on the table row, not a match in the driver loop).
//! [`super::IlModule::optimize_and_flatten`] runs the table per body; every
//! production IL pass is a table row. Fuse-select stays in `lower_optimized`.
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
    /// Promote slots to virtual values (straight-line + same-def joins).
    pub slot_promote: bool,
    /// Operand-order canon (`Const;Load` → `Load;Const`, load/load slot order).
    pub canon: bool,
    /// Algebraic / strength peeps (x+0, x*1, cmp fold, …) when SP Known.
    pub algebraic: bool,
    /// Local CSE in the HIR (`hir::cse`), not an IL pass.
    pub local_cse: bool,
    /// Loop-invariant code motion in the HIR (`hir::licm`), not an IL pass.
    pub licm: bool,
    /// HIR counted-loop in-bounds proofs (`hir::bounds`).
    pub loop_bounds: bool,
    /// Clone plain `RETURN` onto jump-only preds of mixed return joins.
    pub clone_shared_return: bool,
    /// Full-unroll counted natural loops with a known trip count ≤ 8.
    pub loop_unroll: bool,
    /// Cap on trips fully unrolled (clamped to 8). Loops with more trips stay rolled.
    pub loop_unroll_factor: usize,
    /// HIR scalar replacement: split local enums / tuples into field locals
    /// before emit (`hir::enum_sroa`, `hir::tuple_sroa`). Not an IL pass; on
    /// at Standard and above (and Size), off at None / Basic / Debug.
    pub escape_analysis: bool,
    /// Heuristic branch layout (COI-128).
    /// Default **on**: invert only Known-SP terminating then-arms, and mint
    /// labels from a module-wide watermark so ids cannot collide across funcs.
    pub branch_optimization: bool,
    /// Sink jump-only terminating blocks to the end (COI-129). Fall-through
    /// chains stay adjacent; branch labels are not rewritten.
    pub block_reordering: bool,
    /// Record per-pass counters into [`stats::OptStats`] (COI-131). Default **off**.
    pub collect_stats: bool,
    /// Dense specialize + MIR→LIR body replace. On for every named
    /// opt level, including `-Og` (B8). Debugger-attached compiles
    /// keep this on; the VM debugger steps the reconstruct.
    pub mir_specialize: bool,
}

// Default is `OptLevel::Standard.options()` (derived from the driver table).

/// Run IL opts in place. Safe to call before [`super::lower`].
///
/// Pass the const pool when available so canon can read `ConstPool` bits;
/// an empty vec disables the pool-entry demotion.
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
    if opts.collect_stats {
        stats::set_iterations(1);
    }
    driver::run_once(ops, opts, entry_sp, pool);
}

/// Run [`optimize`] on each [`super::IlFunc`] emitting span; leave prologue and
/// inter-function glue untouched. Falls back to whole-buffer opts when `funcs`
/// is empty (unit tests / buffers without `record_func`).
///
/// Thin flat-buffer wrapper over [`super::IlModule::optimize_and_flatten`].
/// Production lower uses [`super::CodeBuf::lower_in_place`] /
/// [`super::lower::lower_module_inner`] on an owning module; this
/// stays for unit tests that mutate a bare `Vec<IlOp>`.
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
///
/// Walk-back stops at a label a prefix `Jump` already targets. Those are
/// the previous function's trailing `if`/`?` end-labels sitting at this PC;
/// pulling them into this span lets MIR/treeshake drop them while the
/// previous body still jumps there (`label was never bound`, COI-407).
pub(crate) fn emitting_range_to_raw(
    ops: &[IlOp],
    emit_start: usize,
    emit_end: usize,
) -> (usize, usize) {
    let mut emitting = 0usize;
    let mut raw_start: Option<usize> = None;
    let mut raw_end: Option<usize> = None;
    for (i, op) in ops.iter().enumerate() {
        if !op.emits_code() {
            continue;
        }
        // Start on the first emitting op, then walk back over this body's
        // leading labels. Trailing if-end of the previous function sits at
        // this same emitting count and must not become `raw_start`.
        if emitting == emit_start && raw_start.is_none() {
            let mut s = i;
            while s > 0 && !ops[s - 1].emits_code() {
                let Some(lab) = ops[s - 1].bind_label() else {
                    break;
                };
                let prefix_jump = ops[..s]
                    .iter()
                    .any(|op| matches!(op, IlOp::Jump { target, .. } if *target == lab));
                if prefix_jump {
                    break;
                }
                s -= 1;
            }
            raw_start = Some(s);
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
mod driver;
mod labels;
mod opt_level;
mod stats;
pub(crate) use labels::{max_code_label, remap_label_space};
pub use opt_level::OptLevel;
pub(crate) use stats::{
    note_body_tier, note_body_tiers, note_branches_optimized, note_fuse_reason, note_hir_fallback,
    note_hir_inline_refused, note_hir_inlined, note_hir_lowered,
};
pub use stats::{BodyTier, OptStats, begin_opt_stats, last_opt_stats};

mod cfg;
mod convoy;
mod dce;
mod slot_promote;


#[cfg(test)]
#[path = "mod.tests.rs"]
mod iterative_opt_tests;
