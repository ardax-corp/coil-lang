//! I6 — HostInvoke / CALL effect edges from the typechecker purity sidecar.
//!
//! W4 dense emit stays the closed math / packed / axpy allowlist. Any other
//! HostInvoke we can type (clocks, IO, GC, FFI names) is a first-class SSA
//! edge under [`crate::mir::LowerHints::allow_effects`]. Impure edges are
//! LICM/CSE barriers and never hoist. Production specialize still refuses
//! non-W4 hosts (no bench-shaped allowlist growth).

use crate::typechecking::purity::{classify_host_name, EffectFlags};

use super::host_allow::host_spec;
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

/// LICM may hoist only W4 scalar-pure math / axpy (not packed LA, not IO).
pub fn host_may_hoist(id: u16) -> bool {
    host_is_pure(id) && host_spec(id).is_some_and(|s| s.hoistable)
}

impl MirInst {
    /// Impure HostInvoke / CALL / alloc / field store — never silently hoist.
    pub fn is_effect_barrier(&self) -> bool {
        match self {
            Self::HostInvoke { native_id, .. } => !host_is_pure(*native_id),
            Self::Call { .. }
            | Self::Alloc { .. }
            | Self::GcBarrier { .. }
            | Self::FieldStore { .. } => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{CLOCK_MONO_NANOS_ID, CLOCK_SLEEP_MS_ID, MATH_SIN_ID, SIMD_AXPY_REDUCE_ID};

    #[test]
    fn math_is_pure_clocks_are_host() {
        assert!(host_is_pure(MATH_SIN_ID));
        assert!(host_may_hoist(MATH_SIN_ID));
        assert!(host_is_pure(SIMD_AXPY_REDUCE_ID));
        assert!(host_may_hoist(SIMD_AXPY_REDUCE_ID));
        assert!(!host_is_pure(CLOCK_MONO_NANOS_ID));
        assert!(!host_may_hoist(CLOCK_MONO_NANOS_ID));
        assert!(!host_is_pure(CLOCK_SLEEP_MS_ID));
        assert!(host_effects(CLOCK_SLEEP_MS_ID).contains(EffectFlags::HOST));
        assert!(!host_is_pure(6)); // write
        assert!(host_effects(6).contains(EffectFlags::IO));
        assert!(!host_is_pure(100)); // gc_collect
        assert!(host_effects(100).contains(EffectFlags::GC));
    }
}
