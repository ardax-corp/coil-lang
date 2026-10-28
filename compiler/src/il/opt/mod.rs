//! Optimization options, levels and stats.
//!
//! No optimization pass runs on the stack IL any more (the last ones went
//! in 2026-10): the HIR and MIR passes do that work, and the stack IL MIR
//! emits is only fuse-selected in `lower_optimized`. See `README.md` in
//! this directory.

use super::op::IlOp;

/// Optimization switches, set per [`OptLevel`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OptimizeOptions {
    /// Constant folding and algebraic identities in the HIR (`hir::fold`).
    pub algebraic: bool,
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
    /// Compute pure calls with constant arguments at compile time
    /// (`Pipeline::compute_consts`, comptime step 4). On at Standard and
    /// above (and Size); `--fold-calls off` turns it off
    /// (`Pipeline::set_call_fold`).
    pub call_fold: bool,
    /// MIR constant propagation and folding before emission
    /// (`mir::specialize`): SCCP, InstCombine and DestProp on dense
    /// bodies, SCCP and InstCombine's folds on LIR bodies. On at every
    /// level but None and Debug.
    pub mir_fold: bool,
    /// MIR redundancy and loop passes (`mir::specialize`): CSE, LICM, IV
    /// strength reduction and GVN/PRE, and the dense saxpy-reduce pack and
    /// vectorizer. On at Standard and above (and Size).
    pub mir_loop: bool,
    /// Typed inlining of HIR bodies (`hir::inline`). Every level; the
    /// inline budget is what turns it off at `-O0` / `-Og`. Always on;
    /// unit tests turn it off.
    pub hir_inline: bool,
    /// Typed inlining takes callees with heap locals, cleared after the
    /// splicing statement. Always on; unit tests turn it off.
    pub hir_inline_heap: bool,
    /// An enum local built in place lives in two slots when its type is a
    /// two-word pair. Always on; unit tests turn it off.
    pub hir_pair_locals: bool,
    /// Lower only the first `N` bodies HIR lowering admits (per process)
    /// and name the `N`th on stderr, to bisect a miscompile.
    /// `COIL_HIR_BISECT=N`; `None` (unset) lowers every body.
    pub hir_bisect: Option<usize>,
    /// Automatic fork-join of pure recursive calls and counted loops
    /// (`Pipeline::set_auto_par` also turns it off). `--auto-par off` off.
    pub auto_par: bool,
    /// Free objects the escape facts prove dead at the end of their scope
    /// or after the call they were passed to (`hir::early_free`). On at
    /// `-O1` and above; `--early-free` overrides the level.
    pub early_free: bool,
    /// Every function must lower through direct MIR: a body left on its
    /// raw stack IL (the fuse tier) is an internal error. The compiler sets
    /// this when codegen reported no errors; IL-only modules leave it off.
    pub require_mir: bool,
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
pub use opt_level::{EarlyFree, OptLevel};
pub(crate) use stats::{
    note_body_tier, note_body_tiers, note_comptime_consts, note_comptime_macros, note_fuse_reason, note_hir_fallback, note_hir_mir, note_hir_mir_kept,
    note_hir_inline_refused, note_hir_inlined, note_hir_lowered,
};
pub use stats::{BodyTier, OptStats, begin_opt_stats, last_opt_stats};


#[cfg(test)]
#[path = "mod.tests.rs"]
mod iterative_opt_tests;
