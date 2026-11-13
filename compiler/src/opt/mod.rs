//! Optimization options, levels and stats.
//!
//! The HIR and MIR passes do the optimizing; this module holds the switches
//! that gate them. See `README.md` in this directory.


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
    /// Compute pure calls with constant arguments at compile time
    /// (`Pipeline::compute_consts`, comptime step 4). On at Standard and
    /// above (and Size); `--fold-calls off` turns it off
    /// (`Pipeline::set_call_fold`).
    pub call_fold: bool,
    /// MIR constant propagation and folding (`mir::specialize`): SCCP,
    /// InstCombine, DestProp and `bool` φ threading. On at every level but
    /// None and Debug.
    pub mir_fold: bool,
    /// MIR redundancy and loop passes (`mir::specialize`): CSE, LICM, loop
    /// interchange, IV strength reduction, GVN, and the loop rewrites
    /// (`copy_until`, `byte_scan`, `bulk_push`, `vec_loops`). On at
    /// Standard and above (and Size).
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
}

// Default is `OptLevel::Standard.options()`.

mod opt_level;
mod stats;
pub use opt_level::{EarlyFree, HintFactors, OptLevel};
pub(crate) use stats::{note_comptime_consts, note_comptime_macros, note_hir_mir, note_hir_refused,
    note_hir_inline_refused, note_hir_inlined, note_hir_lowered,
};
pub use stats::{OptStats, begin_opt_stats, last_opt_stats};


