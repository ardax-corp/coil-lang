//! Typed inlining budgets (`Compiler::hir_inline_round`).

/// How large a callee typed inlining may splice.
#[derive(Clone, Debug)]
pub struct InlineCostOptions {
    /// Default budget for a call site ([`crate::hir::inline::inlinable`]'s cost).
    pub max_inline_cost: usize,
    /// Splice small callees from previously compiled modules (COI-125).
    pub inline_across_modules: bool,
    /// Budget for cross-module callees, weighing every node (a read too)
    /// and a call as 25: stricter than [`Self::max_inline_cost`] at the
    /// same number. 100 takes a `HashMap` lookup's clone.
    pub max_cross_module_inline_cost: usize,
    /// Multiples of these budgets the optimization hints grant.
    pub hint_factors: crate::opt::HintFactors,
}

impl Default for InlineCostOptions {
    fn default() -> Self {
        Self {
            max_inline_cost: 100,
            inline_across_modules: true,
            max_cross_module_inline_cost: 100,
            hint_factors: crate::opt::HintFactors::default(),
        }
    }
}
