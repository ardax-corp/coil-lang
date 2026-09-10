//! Shared Q1–Q4 escape answer for `[T; N]` / `MakeArray` SROA.
//!
//! Codegen, IL [`crate::il::opt`], and MIR `sroa` use the same verdict:
//! proven non-escaping → consecutive slots / scalar SSA; escaping → **box
//! once** and reuse that heap identity; unproven / grow / private-after-escape
//! → stay heap from the start.

/// How a `[T; N]` / `MakeArray` local may be rewritten.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrayEscape {
    /// No whole-object edge. Invisible unbox (frame slots or scalar SSA).
    Private,
    /// Named escape (return, call, field, host, `ArrayPush` value, identity).
    /// Materialize **one** heap object and reuse it on later edges (Q1).
    BoxOnce,
    /// Stay a heap object: grow dest, private use after escape, unproven
    /// `xs[k]` on leftover `MakeArray`, arity > 32.
    Heap,
}

impl ArrayEscape {
    /// Slots / SSA rewrite is sound (private region, or box-at-edge).
    pub fn stack_allocatable(self) -> bool {
        matches!(self, Self::Private | Self::BoxOnce)
    }
}

/// Vec length-changing methods that are a type error on `[T; N]` (Q3).
pub fn is_fixed_array_grow_method(method: &str) -> bool {
    matches!(
        method,
        "push" | "insert" | "pop" | "remove" | "clear" | "reserve"
    )
}
