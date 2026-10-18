//! I8 — which leftover bodies enter MIR→LIR after dense specialize.
//!
//! Both MIR tiers lower from HIR (M4: there is no stack-IL lift). Dense
//! goes first ([`crate::mir::try_specialize_body_side`]); a body it misses
//! takes LIR ([`crate::mir::lower_abi_body_reusing`]) unless its MIR holds
//! a wall ([`super::gates::fuse_lir_wall`]): what LIR cannot emit.

/// Why a leftover body stays fuse-IL instead of MIR→LIR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LirRefuse {
    /// I5: `MakeArray` / `MakeTuple` / `MakeEnum` / `InitTyped`.
    Alloc,
}
