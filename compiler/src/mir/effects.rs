//! I6 — HostInvoke / CALL effect edges from the typechecker purity sidecar.
//!
//! Dense emit reconstructs I6-typed HostInvoke edges, including Q9 R2
//! `from_bytes` / `to_bytes`. Impure edges are LICM/CSE barriers and never hoist. LICM hoist
//! is purity bits (plus heap-read), not a HostInvoke id allowlist.

use crate::typechecking::purity::{classify_host_name, EffectFlags};

use super::inst::MirInst;

/// Sidecar effect bits for HostInvoke native `id`.
pub fn host_effects(id: u16) -> EffectFlags {
    match common::HOST_NATIVES.get(id as usize) {
        Some(n) if n.id == id => classify_host_name(n.name),
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
    /// Impure HostInvoke / CALL / alloc / field store — never silently hoist.
    pub fn is_effect_barrier(&self) -> bool {
        match self {
            Self::HostInvoke { native_id, .. } => !host_is_pure(*native_id),
            Self::Call { .. }
            | Self::Alloc { .. }
            | Self::GcBarrier { .. }
            | Self::Deopt { .. }
            | Self::FieldStore { .. }
            | Self::StoreIndex { .. }
            | Self::ArrayPush { .. }
            | Self::Print { .. }
            | Self::Format { .. }
            | Self::Stringify { .. } => true,
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
        assert!(host_effects(10).contains(EffectFlags::IO));
        assert!(host_effects(11).contains(EffectFlags::IO));
        assert!(!host_may_hoist(10));
        assert!(!host_may_hoist(11));
        assert!(!host_is_pure(100)); // gc_collect
        assert!(host_effects(100).contains(EffectFlags::GC));
    }
}
