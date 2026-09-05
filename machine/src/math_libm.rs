//! IEEE-754 scalar math host natives for virtual `prelude::math`.
//!
//! Remainder is `rem` (Rust `f64::rem` / C `fmod`), not IEEE `remainder`.
//! Named constants `PI` / `E` / `TAU` stay in userland `num`, not HostInvoke.

use common::Value;

use crate::Heap;

pub fn math_sin(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().sin())
}

pub fn math_cos(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().cos())
}

pub fn math_tan(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().tan())
}

pub fn math_sqrt(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().sqrt())
}

pub fn math_floor(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().floor())
}

pub fn math_ceil(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().ceil())
}

pub fn math_exp(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().exp())
}

pub fn math_ln(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().ln())
}

pub fn math_pow(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().powf(args[1].as_float()))
}

pub fn math_atan(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().atan())
}

/// `atan2(y, x)` — same argument order as libm / Rust `f64::atan2`.
pub fn math_atan2(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().atan2(args[1].as_float()))
}

pub fn math_asin(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().asin())
}

pub fn math_acos(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().acos())
}

pub fn math_log10(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().log10())
}

pub fn math_log2(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().log2())
}

pub fn math_cbrt(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().cbrt())
}

/// Float remainder via Rust `f64::rem` (C `fmod`): sign follows the dividend.
pub fn math_rem(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float() % args[1].as_float())
}

pub fn math_sinh(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().sinh())
}

pub fn math_cosh(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().cosh())
}

pub fn math_tanh(_heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(args[0].as_float().tanh())
}

/// Frozen HostInvoke block **102–110**. Do not append here — later ids would slide.
pub const MATH_LIBM_WIRING: &[(&str, usize, fn(&mut Heap, &[Value]) -> Value)] = &[
    ("math_sin", 1, math_sin),
    ("math_cos", 1, math_cos),
    ("math_tan", 1, math_tan),
    ("math_sqrt", 1, math_sqrt),
    ("math_floor", 1, math_floor),
    ("math_ceil", 1, math_ceil),
    ("math_exp", 1, math_exp),
    ("math_ln", 1, math_ln),
    ("math_pow", 2, math_pow),
];

/// M1 expansion, appended after `result_unit_probe` (ids **125–135**).
pub const MATH_LIBM_M1_WIRING: &[(&str, usize, fn(&mut Heap, &[Value]) -> Value)] = &[
    ("math_atan", 1, math_atan),
    ("math_atan2", 2, math_atan2),
    ("math_asin", 1, math_asin),
    ("math_acos", 1, math_acos),
    ("math_log10", 1, math_log10),
    ("math_log2", 1, math_log2),
    ("math_cbrt", 1, math_cbrt),
    ("math_rem", 2, math_rem),
    ("math_sinh", 1, math_sinh),
    ("math_cosh", 1, math_cosh),
    ("math_tanh", 1, math_tanh),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn call(host: fn(&mut Heap, &[Value]) -> Value, args: &[f64]) -> f64 {
        let mut heap = Heap::default();
        let values: Vec<Value> = args.iter().copied().map(Value::from).collect();
        host(&mut heap, &values).as_float()
    }

    #[test]
    fn math_libm_unary_functions_match_f64() {
        let cases: &[(fn(&mut Heap, &[Value]) -> Value, f64, f64)] = &[
            (math_sin, std::f64::consts::FRAC_PI_2, 1.0),
            (math_cos, std::f64::consts::PI, -1.0),
            (math_tan, 0.0, 0.0),
            (math_sqrt, 9.0, 3.0),
            (math_floor, -1.25, -2.0),
            (math_ceil, -1.25, -1.0),
            (math_exp, 1.0, std::f64::consts::E),
            (math_ln, std::f64::consts::E, 1.0),
        ];

        for &(host, input, expected) in cases {
            let actual = call(host, &[input]);
            assert!(
                (actual - expected).abs() < 1e-12,
                "input {input}: expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn math_libm_pow_uses_powf() {
        assert_eq!(call(math_pow, &[2.0, 3.0]), 8.0);
        assert_eq!(call(math_pow, &[9.0, 0.5]), 3.0);
    }

    #[test]
    fn math_libm_m1_unary_functions_match_f64() {
        let cases: &[(fn(&mut Heap, &[Value]) -> Value, f64, f64)] = &[
            (math_atan, 1.0, std::f64::consts::FRAC_PI_4),
            (math_asin, 1.0, std::f64::consts::FRAC_PI_2),
            (math_acos, 1.0, 0.0),
            (math_log10, 1000.0, 3.0),
            (math_log2, 8.0, 3.0),
            (math_cbrt, 27.0, 3.0),
            (math_sinh, 0.0, 0.0),
            (math_cosh, 0.0, 1.0),
            (math_tanh, 0.0, 0.0),
        ];

        for &(host, input, expected) in cases {
            let actual = call(host, &[input]);
            assert!(
                (actual - expected).abs() < 1e-12,
                "input {input}: expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn math_libm_atan2_and_rem_match_f64() {
        assert!((call(math_atan2, &[0.0, -1.0]) - std::f64::consts::PI).abs() < 1e-12);
        assert_eq!(call(math_rem, &[5.5, 2.0]), 5.5 % 2.0);
        assert_eq!(call(math_rem, &[-5.5, 2.0]), -5.5 % 2.0);
    }

    #[test]
    fn math_libm_preserves_ieee_nan_and_infinity() {
        assert!(call(math_sqrt, &[-1.0]).is_nan());
        assert!(call(math_ln, &[-1.0]).is_nan());
        assert_eq!(call(math_ln, &[0.0]), f64::NEG_INFINITY);
        assert_eq!(call(math_exp, &[1000.0]), f64::INFINITY);
        assert!(call(math_pow, &[-2.0, 0.5]).is_nan());
        assert!(call(math_asin, &[2.0]).is_nan());
        assert!(call(math_acos, &[-2.0]).is_nan());
        assert!(call(math_log10, &[-1.0]).is_nan());
        assert!(call(math_log2, &[-1.0]).is_nan());
        assert!(call(math_rem, &[1.0, 0.0]).is_nan());
    }

    #[test]
    fn math_libm_m1_wiring_is_append_only() {
        assert_eq!(MATH_LIBM_WIRING.len(), 9);
        assert_eq!(MATH_LIBM_M1_WIRING[0].0, "math_atan");
        assert_eq!(MATH_LIBM_M1_WIRING.last().map(|w| w.0), Some("math_tanh"));
    }
}
