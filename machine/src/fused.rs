//! Fused inner-op evaluation (same packed `u8` ISA discriminants as `Instruction`).
//!
//! rustc 1.98 has no stable guaranteed tail calls (`become` is nightly), so
//! token-threading the main loop is out. A 256-entry `fn` pointer table was
//! measured ~2% slower on mandelbrot (indirect call on the float fused path).
//! These helpers are `#[inline(always)]` matches so LLVM can emit a jump table
//! of straight-line arms without a second call.

use common::{Instruction, Value};

use crate::Heap;

#[inline(always)]
pub(crate) fn eval_bin(op: u8, lhs: Value, rhs: Value, heap: &Heap) -> Value {
    match Instruction::from(op) {
        Instruction::ADD => Value::from(lhs.as_int() + rhs.as_int()),
        Instruction::SUB => Value::from(lhs.as_int() - rhs.as_int()),
        Instruction::MUL => Value::from(lhs.as_int() * rhs.as_int()),
        Instruction::DIV => Value::from(lhs.as_int() / rhs.as_int()),
        Instruction::MOD => Value::from(lhs.as_int() % rhs.as_int()),
        Instruction::Pow => {
            let exp = rhs.as_int().max(0) as u32;
            Value::from(lhs.as_int().pow(exp))
        }
        Instruction::BITAND => Value::from(lhs.as_int() & rhs.as_int()),
        Instruction::BITOR => Value::from(lhs.as_int() | rhs.as_int()),
        Instruction::SHL => Value::from(lhs.as_int() << rhs.as_int()),
        Instruction::SHR => Value::from(lhs.as_int() >> rhs.as_int()),
        Instruction::XOR => Value::from(lhs.as_int() ^ rhs.as_int()),
        Instruction::AND => Value::from(lhs.as_bool() && rhs.as_bool()),
        Instruction::OR => Value::from(lhs.as_bool() || rhs.as_bool()),
        Instruction::LE => Value::from((lhs.as_int() < rhs.as_int()) as i64),
        Instruction::LEQ => Value::from((lhs.as_int() <= rhs.as_int()) as i64),
        Instruction::GT => Value::from((lhs.as_int() > rhs.as_int()) as i64),
        Instruction::GEQ => Value::from((lhs.as_int() >= rhs.as_int()) as i64),
        Instruction::EQ => Value::from(crate::value_eq::values_eq(heap, lhs, rhs) as i64),
        Instruction::NEQ => Value::from((!crate::value_eq::values_eq(heap, lhs, rhs)) as i64),
        Instruction::ADDF => Value::from(lhs.as_float() + rhs.as_float()),
        Instruction::SUBF => Value::from(lhs.as_float() - rhs.as_float()),
        Instruction::MULF => Value::from(lhs.as_float() * rhs.as_float()),
        Instruction::DIVF => Value::from(lhs.as_float() / rhs.as_float()),
        Instruction::MODF => Value::from(lhs.as_float() % rhs.as_float()),
        Instruction::LEF => Value::from((lhs.as_float() < rhs.as_float()) as i64),
        Instruction::LEQF => Value::from((lhs.as_float() <= rhs.as_float()) as i64),
        Instruction::GTF => Value::from((lhs.as_float() > rhs.as_float()) as i64),
        Instruction::GEQF => Value::from((lhs.as_float() >= rhs.as_float()) as i64),
        Instruction::PowF => Value::from(lhs.as_float().powf(rhs.as_float())),
        _ => Value::default(),
    }
}

#[inline(always)]
pub(crate) fn eval_cmp(op: u8, lhs: Value, rhs: Value, heap: &Heap) -> bool {
    match Instruction::from(op) {
        Instruction::LE => lhs.as_int() < rhs.as_int(),
        Instruction::LEQ => lhs.as_int() <= rhs.as_int(),
        Instruction::GT => lhs.as_int() > rhs.as_int(),
        Instruction::GEQ => lhs.as_int() >= rhs.as_int(),
        Instruction::EQ => crate::value_eq::values_eq(heap, lhs, rhs),
        Instruction::NEQ => !crate::value_eq::values_eq(heap, lhs, rhs),
        Instruction::LEF => lhs.as_float() < rhs.as_float(),
        Instruction::LEQF => lhs.as_float() <= rhs.as_float(),
        Instruction::GTF => lhs.as_float() > rhs.as_float(),
        Instruction::GEQF => lhs.as_float() >= rhs.as_float(),
        Instruction::AND => lhs.as_bool() && rhs.as_bool(),
        Instruction::OR => lhs.as_bool() || rhs.as_bool(),
        Instruction::BITAND => Value::from(lhs.as_int() & rhs.as_int()).as_bool(),
        Instruction::BITOR => Value::from(lhs.as_int() | rhs.as_int()).as_bool(),
        Instruction::XOR => Value::from(lhs.as_int() ^ rhs.as_int()).as_bool(),
        _ => false,
    }
}

#[inline(always)]
pub(crate) fn eval_f64_bin(op: u8, lhs: f64, rhs: f64) -> f64 {
    match Instruction::from(op) {
        Instruction::ADDF => lhs + rhs,
        Instruction::SUBF => lhs - rhs,
        Instruction::MULF => lhs * rhs,
        Instruction::DIVF => lhs / rhs,
        Instruction::MODF => lhs % rhs,
        _ => f64::NAN,
    }
}

#[inline(always)]
pub(crate) fn eval_f64_cmp(op: u8, lhs: f64, rhs: f64) -> bool {
    match Instruction::from(op) {
        Instruction::LEF => lhs < rhs,
        Instruction::LEQF => lhs <= rhs,
        Instruction::GTF => lhs > rhs,
        Instruction::GEQF => lhs >= rhs,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fused_bin_matches_int_arith() {
        let heap = Heap::default();
        let a = Value::from(10i64);
        let b = Value::from(3i64);
        assert_eq!(eval_bin(Instruction::ADD as u8, a, b, &heap).as_int(), 13);
        assert_eq!(eval_bin(Instruction::SUB as u8, a, b, &heap).as_int(), 7);
        assert_eq!(eval_bin(Instruction::MUL as u8, a, b, &heap).as_int(), 30);
        assert_eq!(eval_bin(Instruction::DIV as u8, a, b, &heap).as_int(), 3);
        assert_eq!(eval_bin(Instruction::MOD as u8, a, b, &heap).as_int(), 1);
        assert!(!eval_cmp(Instruction::LE as u8, a, b, &heap));
        assert!(eval_cmp(Instruction::GT as u8, a, b, &heap));
    }

    #[test]
    fn fused_f64_covers_mandelbrot_ops() {
        assert_eq!(eval_f64_bin(Instruction::ADDF as u8, 1.5, 2.25), 3.75);
        assert_eq!(eval_f64_bin(Instruction::MULF as u8, 2.0, 3.0), 6.0);
        assert!(eval_f64_cmp(Instruction::GTF as u8, 5.0, 4.0));
        assert!(!eval_f64_cmp(Instruction::LEQF as u8, 5.0, 4.0));
        let unknown = Instruction::HALT as u8;
        assert!(eval_f64_bin(unknown, 1.0, 1.0).is_nan());
        assert!(!eval_f64_cmp(unknown, 1.0, 1.0));
    }

    #[test]
    fn unknown_packed_op_is_default() {
        let heap = Heap::default();
        let z = eval_bin(
            Instruction::HALT as u8,
            Value::from(1i64),
            Value::from(2i64),
            &heap,
        );
        assert_eq!(z.as_int(), 0);
        assert!(!eval_cmp(
            Instruction::HALT as u8,
            Value::from(1i64),
            Value::from(2i64),
            &heap
        ));
    }
}
