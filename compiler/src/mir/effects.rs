//! I6 — HostInvoke / CALL effect edges from the typechecker purity sidecar.
//!
//! Dense emit reconstructs I6-typed HostInvoke edges, including Q9 R2
//! `from_bytes` / `to_bytes`. Impure edges are LICM/CSE barriers and never hoist. LICM hoist
//! is purity bits (plus heap-read), not a HostInvoke id allowlist.

use crate::typechecking::purity::EffectFlags;

use std::collections::HashSet;

use super::inst::{MirInst, ValueId};

/// What a body's calls may do: [`MirFunc::pure_calls`],
/// [`MirFunc::steady_calls`], [`MirFunc::lengths_steady`].
///
/// [`MirFunc::pure_calls`]: super::func::MirFunc::pure_calls
/// [`MirFunc::steady_calls`]: super::func::MirFunc::steady_calls
/// [`MirFunc::lengths_steady`]: super::func::MirFunc::lengths_steady
pub struct CallFacts<'a> {
    pub pure: &'a HashSet<ValueId>,
    pub steady: &'a HashSet<ValueId>,
    pub lengths_steady: bool,
}

impl super::func::MirFunc {
    pub fn call_facts(&self) -> CallFacts<'_> {
        CallFacts { pure: &self.pure_calls, steady: &self.steady_calls, lengths_steady: self.lengths_steady }
    }
}

/// Sidecar effect bits for HostInvoke native `id`.
pub fn host_effects(id: u16) -> EffectFlags {
    match common::HOST_NATIVES.get(id as usize) {
        Some(n) if n.id == id => n.effects,
        _ => EffectFlags::from_bits(EffectFlags::UNKNOWN | EffectFlags::HOST),
    }
}

pub fn host_is_pure(id: u16) -> bool {
    host_effects(id).is_pure()
}

/// LICM may hoist scalar-pure math / axpy. Packed LA reads heap aliases
/// and stays in place even though [`host_is_pure`] is true.
pub fn host_may_hoist(id: u16) -> bool {
    if !host_is_pure(id) {
        return false;
    }
    match common::HOST_NATIVES.get(id as usize) {
        Some(n) if n.id == id => !n.name.starts_with("packed_"),
        _ => false,
    }
}

impl MirInst {
    /// Int arithmetic or negation (overflow, a zero divisor, a negative
    /// exponent) or a checked
    /// index: an op that can panic, so it runs even when its value is
    /// unused (#925).
    pub fn may_trap(&self, func: &super::func::MirFunc) -> bool {
        use super::inst::{MirBinOp, MirUnaryOp};
        use super::ty::MirTy;
        match self {
            Self::Bin { op, ty, .. } => {
                *ty == MirTy::I64
                    && matches!(
                        op,
                        MirBinOp::Add | MirBinOp::Sub | MirBinOp::Mul | MirBinOp::Div | MirBinOp::Rem | MirBinOp::Pow | MirBinOp::Shl | MirBinOp::Shr
                    )
            }
            Self::Unary { op: MirUnaryOp::Neg, src, .. } => func.ty(*src) == MirTy::I64,
            Self::Index { unchecked, .. } => !unchecked,
            Self::VecLoop { .. } => true,
            _ => false,
        }
    }

    /// Code that may run user code with effects: an impure call or
    /// native, a coroutine step, an append, a `Show` format.
    fn runs_effects(&self, calls: &CallFacts) -> bool {
        match self {
            Self::Call { dest, .. } => !calls.pure.contains(dest),
            Self::HostInvoke { native_id, .. } => !host_is_pure(*native_id),
            Self::CallIndirect { .. }
            | Self::Ffi { .. }
            | Self::Yield { .. }
            | Self::ResumeCoro { .. }
            | Self::ArrayPush { .. }
            | Self::Format { .. }
            | Self::Stringify { .. }
            | Self::Cow { .. } => true,
            _ => false,
        }
    }

    /// May change a word an [`MirInst::Index`] reads: an element store, or
    /// code that may store. The VM's element ops (`StoreIndex`, its pin
    /// and dense forms) write only `Object::Array` slots, and field ops
    /// (`SetField`, `DenseFieldStore`) only `Object::Instance` slots, so
    /// neither kind of store changes what a read of the other sees (#1559).
    pub fn writes_elements(&self, calls: &CallFacts) -> bool {
        self.runs_effects(calls)
            || match self {
                Self::StoreIndex { .. } | Self::VecLoop { kind: super::inst::VecLoopKind::Store { .. }, .. } => true,
                Self::VecLoop { kind: super::inst::VecLoopKind::Reduce { stores, .. }, .. } => !stores.is_empty(),
                _ => false,
            }
    }

    /// May change a word a [`MirInst::HeapFieldLoad`] reads (an instance
    /// slot or an enum payload word): a field store, or code that may
    /// store. See [`Self::writes_elements`].
    pub fn writes_fields(&self, calls: &CallFacts) -> bool {
        self.runs_effects(calls) || matches!(self, Self::HeapFieldStore { .. })
    }

    /// May change an array's length: an append, code that may append (a
    /// call of a function that is neither pure nor steady), or, when
    /// lengths are not steady, an allocation a finalizer may run at.
    pub fn may_resize(&self, calls: &CallFacts) -> bool {
        match self {
            Self::Call { dest, .. } => !calls.lengths_steady || !calls.pure.contains(dest) && !calls.steady.contains(dest),
            Self::Alloc { .. } | Self::GcBarrier { .. } => !calls.lengths_steady,
            _ => self.runs_effects(calls),
        }
    }

    /// Impure HostInvoke / CALL / alloc / field store — never silently hoist.
    pub fn is_effect_barrier(&self) -> bool {
        match self {
            Self::HostInvoke { native_id, .. } => !host_is_pure(*native_id),
            Self::Ffi { .. }
            | Self::Call { .. }
            | Self::CallIndirect { .. }
            | Self::ResumeCoro { .. }
            | Self::Yield { .. }
            | Self::Alloc { .. }
            | Self::GcBarrier { .. }
            | Self::Deopt { .. }
            | Self::HeapFieldStore { .. }
            | Self::StoreIndex { .. }
            | Self::StoreStatic { .. }
            | Self::HomeStore { .. }
            | Self::ClearWord { .. }
            | Self::KeepAlive { .. }
            | Self::ArrayPush { .. }
            | Self::Print { .. }
            | Self::FreeEarly { .. }
            | Self::Panic { .. }
            | Self::Format { .. }
            | Self::Stringify { .. }
            | Self::VecLoop { .. }
            | Self::Cow { .. } => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{
        CLOCK_MONO_NANOS_ID, CLOCK_SLEEP_MS_ID, MATH_SIN_ID, PACKED_DOT_ID, SIMD_AXPY_REDUCE_ID,
    };

    #[test]
    fn math_is_pure_clocks_are_host() {
        assert!(host_is_pure(MATH_SIN_ID));
        assert!(host_may_hoist(MATH_SIN_ID));
        assert!(host_is_pure(SIMD_AXPY_REDUCE_ID));
        assert!(host_may_hoist(SIMD_AXPY_REDUCE_ID));
        assert!(host_is_pure(PACKED_DOT_ID));
        assert!(!host_may_hoist(PACKED_DOT_ID));
        assert!(!host_is_pure(CLOCK_MONO_NANOS_ID));
        assert!(!host_may_hoist(CLOCK_MONO_NANOS_ID));
        assert!(!host_is_pure(CLOCK_SLEEP_MS_ID));
        assert!(host_effects(CLOCK_SLEEP_MS_ID).contains(EffectFlags::HOST));
        assert!(!host_is_pure(6)); // write
        assert!(host_effects(6).contains(EffectFlags::IO));
        assert!(!host_is_pure(10)); // from_bytes
        assert!(!host_is_pure(11)); // to_bytes
        assert!(host_effects(10).contains(EffectFlags::ALLOC));
        assert!(host_effects(11).contains(EffectFlags::ALLOC));
        assert!(!host_may_hoist(10));
        assert!(!host_may_hoist(11));
        assert!(!host_is_pure(100)); // gc_collect
        assert!(host_effects(100).contains(EffectFlags::GC));
    }
}
