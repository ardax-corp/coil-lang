//! Fused inner-op evaluation (same packed `u8` ISA discriminants as `Instruction`).
//!
//! rustc 1.98 has no stable guaranteed tail calls (`become` is nightly), so
//! token-threading the main loop is out. A 256-entry `fn` pointer table was
//! measured ~2% slower on mandelbrot (indirect call on the float fused path).
//! These helpers are `#[inline(always)]` matches so LLVM can emit a jump table
//! of straight-line arms without a second call.

use common::int_arith::IntTrap;
use common::{Instruction, Value};

use crate::arith::{self, Cmp, FloatOp, IntOp};
use crate::{Heap, HeapSlot};

/// Heap pointer for fused ops. `HeapSlot::get` branches on the steal borrow;
/// integer arms must not call it. Equality goes through [`eq_values`], which
/// is outlined so that branch stays off the arithmetic jump table.
pub(crate) trait HeapView {
    fn as_heap(&self) -> &Heap;
}

impl HeapView for Heap {
    #[inline(always)]
    fn as_heap(&self) -> &Heap {
        self
    }
}

impl HeapView for HeapSlot {
    #[inline(always)]
    fn as_heap(&self) -> &Heap {
        self.get()
    }
}

/// Outlined so `HeapSlot::get` is not hoisted above the integer arms.
/// Two unequal words of which one lies outside the slab's mapped span (an
/// `int` compare, mostly) answer from that span check alone, before
/// [`crate::value_eq::values_eq`] probes the heap for both (#1866).
#[inline(never)]
fn eq_values<H: HeapView>(heap: &H, lhs: Value, rhs: Value) -> bool {
    if lhs.raw() == rhs.raw() {
        return true;
    }
    let heap = heap.as_heap();
    if !heap.may_be_ref(lhs) || !heap.may_be_ref(rhs) {
        return false;
    }
    crate::value_eq::values_eq(heap, lhs, rhs)
}

/// A fused binary op; an int op that overflows or divides by zero traps.
/// The semantics live in [`crate::arith`]; this maps the packed op. The
/// plain stack handlers call it too, with a constant op.
#[inline(always)]
pub(crate) fn eval_bin<H: HeapView>(op: u8, lhs: Value, rhs: Value, heap: &H) -> Result<Value, IntTrap> {
    let int = |op| arith::int(op, lhs.as_int(), rhs.as_int()).map(Value::from);
    let float = |op| Value::from(arith::float(op, lhs.as_float(), rhs.as_float()));
    let icmp = |c| Value::from(arith::cmp(c, lhs.as_int(), rhs.as_int()) as i64);
    let fcmp = |c| Value::from(arith::cmp(c, lhs.as_float(), rhs.as_float()) as i64);
    Ok(match Instruction::from(op) {
        Instruction::ADD => return int(IntOp::Add),
        Instruction::SUB => return int(IntOp::Sub),
        Instruction::MUL => return int(IntOp::Mul),
        Instruction::DIV => return int(IntOp::Div),
        Instruction::MOD => return int(IntOp::Rem),
        Instruction::Pow => return int(IntOp::Pow),
        Instruction::BITAND => return int(IntOp::BitAnd),
        Instruction::BITOR => return int(IntOp::BitOr),
        Instruction::SHL => return int(IntOp::Shl),
        Instruction::SHR => return int(IntOp::Shr),
        Instruction::XOR => return int(IntOp::Xor),
        Instruction::AND => Value::from(arith::and(lhs, rhs)),
        Instruction::OR => Value::from(arith::or(lhs, rhs)),
        Instruction::LE => icmp(Cmp::Lt),
        Instruction::LEQ => icmp(Cmp::Le),
        Instruction::GT => icmp(Cmp::Gt),
        Instruction::GEQ => icmp(Cmp::Ge),
        Instruction::EQ => Value::from(eq_values(heap, lhs, rhs) as i64),
        Instruction::NEQ => Value::from((!eq_values(heap, lhs, rhs)) as i64),
        Instruction::ADDF => float(FloatOp::Add),
        Instruction::SUBF => float(FloatOp::Sub),
        Instruction::MULF => float(FloatOp::Mul),
        Instruction::DIVF => float(FloatOp::Div),
        Instruction::MODF => float(FloatOp::Rem),
        Instruction::LEF => fcmp(Cmp::Lt),
        Instruction::LEQF => fcmp(Cmp::Le),
        Instruction::EQF => fcmp(Cmp::Eq),
        Instruction::NEQF => fcmp(Cmp::Ne),
        Instruction::GTF => fcmp(Cmp::Gt),
        Instruction::GEQF => fcmp(Cmp::Ge),
        Instruction::PowF => float(FloatOp::Pow),
        _ => Value::default(),
    })
}

/// A fused compare-branch condition: the same ops as [`eval_bin`], read as
/// a `bool` word.
#[inline(always)]
pub(crate) fn eval_cmp<H: HeapView>(op: u8, lhs: Value, rhs: Value, heap: &H) -> bool {
    let icmp = |c| arith::cmp(c, lhs.as_int(), rhs.as_int());
    let fcmp = |c| arith::cmp(c, lhs.as_float(), rhs.as_float());
    let bits = |op| arith::int(op, lhs.as_int(), rhs.as_int()).is_ok_and(|r| Value::from(r).as_bool());
    match Instruction::from(op) {
        Instruction::LE => icmp(Cmp::Lt),
        Instruction::LEQ => icmp(Cmp::Le),
        Instruction::GT => icmp(Cmp::Gt),
        Instruction::GEQ => icmp(Cmp::Ge),
        Instruction::EQ => eq_values(heap, lhs, rhs),
        Instruction::NEQ => !eq_values(heap, lhs, rhs),
        Instruction::LEF => fcmp(Cmp::Lt),
        Instruction::LEQF => fcmp(Cmp::Le),
        Instruction::EQF => fcmp(Cmp::Eq),
        Instruction::NEQF => fcmp(Cmp::Ne),
        Instruction::GTF => fcmp(Cmp::Gt),
        Instruction::GEQF => fcmp(Cmp::Ge),
        Instruction::AND => arith::and(lhs, rhs),
        Instruction::OR => arith::or(lhs, rhs),
        Instruction::BITAND => bits(IntOp::BitAnd),
        Instruction::BITOR => bits(IntOp::BitOr),
        Instruction::XOR => bits(IntOp::Xor),
        _ => false,
    }
}

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
