//! Per-op operand-height deltas over IL ops.
//!
//! This is **not** the shared buffer cursor: [`super::tell`] tracks `tell`,
//! which `STORE` floors to `slot + 1` even when height is lower.
//! [`stack_delta`] only counts eval-stack values; it feeds [`super::tell`] and
//! `coil dissect`'s operand-depth column. Do not substitute tell here — a
//! STORE floor is not height (COI-81).



#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::op::Label;
    use common::DebugLoc;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

}
