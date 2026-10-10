//! Optimization options, levels and stats.
//!
//! No optimization pass runs on the stack IL any more (the last ones went
//! in 2026-10): the HIR passes and HIR lowering do that work, and the stack
//! IL only lifts to MIR and fuse-selects in `lower_optimized`. See
//! `README.md` in this directory.

use super::op::IlOp;

/// Optimization switches, set per [`OptLevel`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OptimizeOptions {
    /// Constant folding and algebraic identities in the HIR (`hir::fold`).
    pub algebraic: bool,
    /// Local CSE in the HIR (`hir::cse`).
    pub local_cse: bool,
    /// Loop-invariant code motion in the HIR (`hir::licm`).
    pub licm: bool,
    /// HIR counted-loop in-bounds proofs (`hir::bounds`).
    pub loop_bounds: bool,
    /// Return in each branch of a returned `match` / `if` in the HIR
    /// (`hir::sink_return`).
    pub sink_return: bool,
    /// Full unroll of counted loops with a known trip count ≤ 8 in the HIR
    /// (`hir::unroll`).
    pub loop_unroll: bool,
    /// Cap on trips fully unrolled (clamped to 8). Loops with more trips stay rolled.
    pub loop_unroll_factor: usize,
    /// HIR scalar replacement: split local enums / tuples into field locals
    /// before emit (`hir::enum_sroa`, `hir::tuple_sroa`). On at Standard and
    /// above (and Size), off at None / Basic / Debug.
    pub escape_analysis: bool,
    /// Lay out early exits after the function body in HIR lowering
    /// (`emit_hir`, COI-128).
    pub branch_optimization: bool,
    /// Record body tiers and HIR counters into [`stats::OptStats`] (COI-131).
    /// Default **off**.
    pub collect_stats: bool,
    /// Dense specialize + MIR→LIR body replace. On for every named
    /// opt level, including `-Og` (B8). Debugger-attached compiles
    /// keep this on; the VM debugger steps the reconstruct.
    pub mir_specialize: bool,
}

// Default is `OptLevel::Standard.options()`.

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


#[cfg(test)]
#[path = "mod.tests.rs"]
mod iterative_opt_tests;
