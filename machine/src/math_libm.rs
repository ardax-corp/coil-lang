//! Retired `prelude::math` HostInvoke slots.
//!
//! The scalar float functions are coil-stdlib's userland `math` module now.
//! Their ids stay frozen (see `common::HOST_NATIVES`), so each slot is a
//! panic stub: an archive compiled before the retirement fails loudly when
//! it reaches one instead of every later id sliding. `math_pow` / `math_rem`
//! lost their surface earlier, to the `**` and `%` operators.

/// Frozen math slots after `gc_register_finalizer` (ids **102–110**).
pub const MATH_LIBM_REMOVED: &[(&str, usize)] = &[
    ("math_sin", 1),
    ("math_cos", 1),
    ("math_tan", 1),
    ("math_sqrt", 1),
    ("math_floor", 1),
    ("math_ceil", 1),
    ("math_exp", 1),
    ("math_ln", 1),
    ("math_pow", 2),
];

/// M1 expansion slots after `result_unit_probe` (ids **125–135**).
pub const MATH_LIBM_M1_REMOVED: &[(&str, usize)] = &[
    ("math_atan", 1),
    ("math_atan2", 2),
    ("math_asin", 1),
    ("math_acos", 1),
    ("math_log10", 1),
    ("math_log2", 1),
    ("math_cbrt", 1),
    ("math_rem", 2),
    ("math_sinh", 1),
    ("math_cosh", 1),
    ("math_tanh", 1),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removed_math_slots_cover_both_frozen_blocks() {
        assert_eq!(MATH_LIBM_REMOVED.len(), 9);
        assert_eq!(MATH_LIBM_REMOVED[0].0, "math_sin");
        assert_eq!(MATH_LIBM_M1_REMOVED[0].0, "math_atan");
        assert_eq!(MATH_LIBM_M1_REMOVED.last().map(|w| w.0), Some("math_tanh"));
        for &(name, arity) in MATH_LIBM_REMOVED.iter().chain(MATH_LIBM_M1_REMOVED) {
            let row = common::host_native_id(name).map(|id| &common::HOST_NATIVES[id]);
            assert_eq!(row.map(|r| r.arity as usize), Some(arity), "{name}");
        }
    }
}
