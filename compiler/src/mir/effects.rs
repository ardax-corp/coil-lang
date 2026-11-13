//! I6 — HostInvoke / CALL effect edges from the typechecker purity sidecar.
//!
//! MIR lowering types each HostInvoke edge by its I6 effects, including Q9
//! R2 `from_bytes` / `to_bytes`. Impure edges are LICM/CSE barriers and never hoist. LICM hoist
//! is purity bits (plus heap-read), not a HostInvoke id allowlist.

use common::EffectFlags;

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

/// Whether an op may run user code with effects ([`OpProps::effects`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effects {
    No,
    Yes,
    /// A direct call: unless the callee is pure ([`CallFacts::pure`]).
    UnlessPureCall,
}

/// Whether an op may panic ([`OpProps::traps`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Traps {
    No,
    Yes,
    /// A negation: when its operand is an `int`.
    IfIntOperand,
}

/// What an op may do, as the passes that move, merge, drop or vectorize
/// code ask it. [`MirInst::props`] has one row per op and no fall-through,
/// so a new op decides each column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpProps {
    /// Never hoisted, merged or deleted, even when its value is unused
    /// ([`MirInst::is_effect_barrier`]).
    pub barrier: bool,
    /// May run user code with effects ([`MirInst::runs_effects`]).
    pub effects: Effects,
    /// May panic, so it runs even when its value is unused
    /// ([`MirInst::may_trap`]).
    pub traps: Traps,
    /// An allocation or GC safepoint: specialize and native code must not
    /// move code across it ([`MirInst::is_gc_edge`]).
    pub gc_edge: bool,
    /// Its runtime helper may allocate, so a body holding it may collect
    /// ([`super::frame_words::allocates`]).
    pub allocates: bool,
    /// A vector loop body may not hold it (with any [`Self::gc_edge`]).
    pub vector_barrier: bool,
}

impl OpProps {
    /// An op that only computes its value.
    pub const PURE: Self = Self { barrier: false, effects: Effects::No, traps: Traps::No, gc_edge: false, allocates: false, vector_barrier: false };
}

impl MirInst {
    /// This op's row of the property table ([`OpProps`]).
    pub fn props(&self) -> OpProps {
        use super::inst::{MirBinOp, MirUnaryOp, VecLoopKind};
        use super::ty::MirTy;
        const P: OpProps = OpProps::PURE;
        match self {
            Self::Const { .. } | Self::Cmp { .. } | Self::Cast { .. } | Self::Phi { .. } | Self::ArrayLen { .. } | Self::DoneCoro { .. } | Self::ByteScan { .. } => P,
            // Int arithmetic traps on overflow, a zero divisor or a negative
            // exponent (#925); bit ops and floats never trap.
            Self::Bin { op, ty, .. } => OpProps {
                traps: if *ty == MirTy::I64 && !matches!(op, MirBinOp::BitAnd | MirBinOp::BitOr | MirBinOp::Xor) { Traps::Yes } else { Traps::No },
                ..P
            },
            Self::Unary { op, .. } => OpProps { traps: if *op == MirUnaryOp::Neg { Traps::IfIntOperand } else { Traps::No }, ..P },
            Self::Index { unchecked, .. } => OpProps { traps: if *unchecked { Traps::No } else { Traps::Yes }, ..P },
            Self::LaneIndex { .. } => OpProps { traps: Traps::Yes, vector_barrier: true, ..P },
            Self::StrEq { .. } => OpProps { vector_barrier: true, ..P },
            // Counts a run: nothing reads its token.
            Self::Probe { .. } => OpProps { barrier: true, vector_barrier: true, ..P },
            Self::MatchPayload { .. } | Self::ProductLoad { .. } | Self::CodePtr { .. } | Self::LoadStatic { .. } | Self::HomeLoad { .. } => {
                OpProps { vector_barrier: true, ..P }
            }
            Self::String { .. } => OpProps { allocates: true, vector_barrier: true, ..P },
            Self::HostInvoke { native_id, .. } => {
                let impure = !host_is_pure(*native_id);
                OpProps { barrier: impure, effects: if impure { Effects::Yes } else { Effects::No }, allocates: true, vector_barrier: true, ..P }
            }
            Self::Call { .. } => OpProps { barrier: true, effects: Effects::UnlessPureCall, vector_barrier: true, ..P },
            Self::CallIndirect { .. } | Self::Ffi { .. } => {
                OpProps { barrier: true, effects: Effects::Yes, allocates: true, vector_barrier: true, ..P }
            }
            Self::ResumeCoro { .. } | Self::Yield { .. } => OpProps { barrier: true, effects: Effects::Yes, allocates: true, vector_barrier: true, ..P },
            Self::Alloc { .. } => OpProps { barrier: true, gc_edge: true, allocates: true, ..P },
            Self::GcBarrier { .. } => OpProps { barrier: true, gc_edge: true, ..P },
            Self::ArrayPush { .. } | Self::LanePush { .. } => OpProps { barrier: true, effects: Effects::Yes, gc_edge: true, allocates: true, ..P },
            Self::LanesToWords { .. } => OpProps { barrier: true, gc_edge: true, allocates: true, ..P },
            // A share sets the object's flag and allocates nothing (native
            // code does it inline, #2506): no collection there.
            Self::Cow { unique: false, field: None, .. } => OpProps { barrier: true, effects: Effects::Yes, vector_barrier: true, ..P },
            Self::Format { .. } | Self::Stringify { .. } | Self::Cow { .. } => {
                OpProps { barrier: true, effects: Effects::Yes, gc_edge: true, allocates: true, vector_barrier: true, ..P }
            }
            Self::StoreIndex { .. } => OpProps { barrier: true, ..P },
            Self::LaneStore { .. } => OpProps { barrier: true, traps: Traps::Yes, vector_barrier: true, ..P },
            Self::HeapFieldStore { .. } | Self::StoreStatic { .. } | Self::HomeStore { .. } | Self::ClearWord { .. } | Self::KeepAlive { .. } | Self::Print { .. } | Self::Panic { .. } => {
                OpProps { barrier: true, vector_barrier: true, ..P }
            }
            Self::FreeEarly { .. } => OpProps { barrier: true, allocates: true, vector_barrier: true, ..P },
            Self::VecLoop { kind, .. } => OpProps {
                barrier: true,
                effects: if matches!(kind, VecLoopKind::Push { .. }) { Effects::Yes } else { Effects::No },
                traps: Traps::Yes,
                ..P
            },
        }
    }

    /// An op that can panic (int arithmetic or negation, a checked index,
    /// a lane access), so it runs even when its value is unused (#925).
    pub fn may_trap(&self, func: &super::func::MirFunc) -> bool {
        match self.props().traps {
            Traps::No => false,
            Traps::Yes => true,
            Traps::IfIntOperand => matches!(self, Self::Unary { src, .. } if func.ty(*src) == super::ty::MirTy::I64),
        }
    }

    /// Code that may run user code with effects: an impure call or
    /// native, a coroutine step, an append, a `Show` format.
    fn runs_effects(&self, calls: &CallFacts) -> bool {
        match self.props().effects {
            Effects::No => false,
            Effects::Yes => true,
            Effects::UnlessPureCall => !calls.pure.contains(&self.dest()),
        }
    }

    /// May change a word an [`MirInst::Index`] reads: an element store, or
    /// code that may store. The runtime's element ops (`StoreIndex`, its pin
    /// and dense forms) write only `Object::Array` slots, and field ops
    /// (`SetField`, `DenseFieldStore`) only `Object::Instance` slots, so
    /// neither kind of store changes what a read of the other sees (#1559).
    pub fn writes_elements(&self, calls: &CallFacts) -> bool {
        self.runs_effects(calls)
            || match self {
                Self::StoreIndex { .. } | Self::LaneStore { .. } | Self::LanesToWords { .. } | Self::VecLoop { kind: super::inst::VecLoopKind::Store { .. }, .. } => true,
                Self::VecLoop { kind: super::inst::VecLoopKind::Reduce { stores, .. }, .. } => !stores.is_empty(),
                _ => false,
            }
    }

    /// May change a word a [`MirInst::ProductLoad`] reads (an instance
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

    /// Impure HostInvoke / CALL / alloc / store / output: never hoisted,
    /// merged or dropped.
    pub fn is_effect_barrier(&self) -> bool {
        self.props().barrier
    }

    /// Allocation or GC safepoint: specialize / native must not cross.
    pub fn is_gc_edge(&self) -> bool {
        self.props().gc_edge
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
