//! Fused inner-op evaluation (same packed `u8` ISA discriminants as `Instruction`).
//!
//! rustc 1.98 has no stable guaranteed tail calls (`become` is nightly), so
//! token-threading the main loop is out. A 256-entry `fn` pointer table was
//! measured ~2% slower on mandelbrot (indirect call on the float fused path).
//! These helpers are `#[inline(always)]` matches so LLVM can emit a jump table
//! of straight-line arms without a second call.

use common::int_arith::IntTrap;
use common::{Instruction, Value};

use crate::arith::{self, };
use crate::{Heap, HeapSlot};

#[cfg(test)]
mod tests {
    use super::*;

    /// #1866: an int compare answers from the slab span; heap words still
    /// compare structurally (two strings by content, Err tags included).
    #[test]
    fn fused_eq_ints_and_heap_words() {
        use crate::memory::{ObjString, Object};
        let mut heap = Heap::default();
        let eq = |h: &Heap, a: Value, b: Value| eval_cmp(Instruction::EQ as u8, a, b, h);
        assert!(!eq(&heap, Value::from(1_234_568_i64), Value::from(1_234_570_i64)));
        assert!(eq(&heap, Value::from(1_234_568_i64), Value::from(1_234_568_i64)));
        let (a, _) = heap.alloc(ObjString::new("same".to_string()), Object::String);
        let (b, _) = heap.alloc(ObjString::new("same".to_string()), Object::String);
        let (wa, wb) = (Value::from(a.addr()), Value::from(b.addr()));
        assert!(eq(&heap, wa, wb));
        assert!(!eq(&heap, wa, Value::from(4_i64)));
        assert!(eq(&heap, Value::from(a.addr() | 1), Value::from(b.addr() | 1)));
        assert!(!eval_cmp(Instruction::NEQ as u8, wa, wb, &heap));
    }

    /// `IEQ` / `INEQ` compare the words bit for bit, with no heap probe:
    /// two equal strings at different addresses are not `IEQ`.
    #[test]
    fn immediate_eq_compares_words() {
        use crate::memory::{ObjString, Object};
        let mut heap = Heap::default();
        let ieq = |h: &Heap, a: Value, b: Value| eval_cmp(Instruction::IEQ as u8, a, b, h);
        assert!(ieq(&heap, Value::from(7_i64), Value::from(7_i64)));
        assert!(!ieq(&heap, Value::from(7_i64), Value::from(8_i64)));
        assert!(eval_cmp(Instruction::INEQ as u8, Value::from(7_i64), Value::from(8_i64), &heap));
        let (a, _) = heap.alloc(ObjString::new("same".to_string()), Object::String);
        let (b, _) = heap.alloc(ObjString::new("same".to_string()), Object::String);
        assert!(!ieq(&heap, Value::from(a.addr()), Value::from(b.addr())));
        assert_eq!(eval_bin(Instruction::IEQ as u8, Value::from(-1_i64), Value::from(-1_i64), &heap), Ok(Value::from(1_i64)));
    }

    #[test]
    fn fused_bin_matches_int_arith() {
        let heap = Heap::default();
        let a = Value::from(10i64);
        let b = Value::from(3i64);
        assert_eq!(eval_bin(Instruction::ADD as u8, a, b, &heap).unwrap().as_int(), 13);
        assert_eq!(eval_bin(Instruction::SUB as u8, a, b, &heap).unwrap().as_int(), 7);
        assert_eq!(eval_bin(Instruction::MUL as u8, a, b, &heap).unwrap().as_int(), 30);
        assert_eq!(eval_bin(Instruction::DIV as u8, a, b, &heap).unwrap().as_int(), 3);
        assert_eq!(eval_bin(Instruction::MOD as u8, a, b, &heap).unwrap().as_int(), 1);
        assert!(!eval_cmp(Instruction::LE as u8, a, b, &heap));
        assert!(eval_cmp(Instruction::GT as u8, a, b, &heap));
    }

    #[test]
    fn fused_int_ops_trap() {
        let heap = Heap::default();
        let max = Value::from(i64::MAX);
        let one = Value::from(1i64);
        let zero = Value::from(0i64);
        assert_eq!(eval_bin(Instruction::ADD as u8, max, one, &heap), Err(IntTrap::Overflow));
        assert_eq!(eval_bin(Instruction::MUL as u8, max, max, &heap), Err(IntTrap::Overflow));
        assert_eq!(eval_bin(Instruction::DIV as u8, one, zero, &heap), Err(IntTrap::DivByZero));
        assert_eq!(eval_bin(Instruction::Pow as u8, one, Value::from(-1i64), &heap), Err(IntTrap::NegativeExponent));
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
        assert_eq!(z.unwrap().as_int(), 0);
        assert!(!eval_cmp(
            Instruction::HALT as u8,
            Value::from(1i64),
            Value::from(2i64),
            &heap
        ));
    }
}
