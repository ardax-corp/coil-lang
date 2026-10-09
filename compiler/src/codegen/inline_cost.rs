//! Typed inlining budgets (`Compiler::hir_inline_replan`).

/// How large a callee typed inlining may splice.
#[derive(Clone, Debug)]
pub struct InlineCostOptions {
    /// Default budget for a call site ([`crate::hir::inline::inlinable`]'s cost).
    pub max_inline_cost: usize,
    /// Splice small callees from previously compiled modules (COI-125).
    pub inline_across_modules: bool,
    /// Stricter budget than [`Self::max_inline_cost`] for cross-module
    /// callees, weighing every node and a call as 25.
    pub max_cross_module_inline_cost: usize,
}

impl Default for InlineCostOptions {
    fn default() -> Self {
        Self {
            max_inline_cost: 100,
            inline_across_modules: true,
            max_cross_module_inline_cost: 50,
        }
    }
}
