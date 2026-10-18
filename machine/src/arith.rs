//! Scalar binary-op semantics, defined once for every VM path: the plain
//! stack handlers (`exec_rest`), the fused slot ops ([`crate::fused`]) and
//! the dense kernels ([`crate::dense`]).
//!
//! Each path keeps its own operand decode and its own op-code match (the
//! packed `Instruction` discriminant, or a dense kind byte); every arm names
//! an op here by a constant. These are `#[inline(always)]`, so the inner
//! match folds away and each path still dispatches through one jump table.
//! `int` ops trap through [`common::int_arith`].

use common::Value;
use common::int_arith::{self, IntResult};

/// A binary op on two `int` words.
#[derive(Clone, Copy)]
pub(crate) enum IntOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    BitAnd,
    BitOr,
    Xor,
    Shl,
    Shr,
}

/// A binary op on two `float` words.
#[derive(Clone, Copy)]
pub(crate) enum FloatOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
}

/// An ordering or equality predicate.
#[derive(Clone, Copy)]
pub(crate) enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

/// The exact `int` result, or the trap (overflow, division by zero, a
/// negative exponent, a shift count outside `0..=63`).
#[inline(always)]
pub(crate) fn int(op: IntOp, a: i64, b: i64) -> IntResult {
    match op {
        IntOp::Add => int_arith::add(a, b),
        IntOp::Sub => int_arith::sub(a, b),
        IntOp::Mul => int_arith::mul(a, b),
        IntOp::Div => int_arith::div(a, b),
        IntOp::Rem => int_arith::rem(a, b),
        IntOp::Pow => int_arith::pow(a, b),
        IntOp::BitAnd => Ok(a & b),
        IntOp::BitOr => Ok(a | b),
        IntOp::Xor => Ok(a ^ b),
        IntOp::Shl => int_arith::shl(a, b),
        IntOp::Shr => int_arith::shr(a, b),
    }
}

/// IEEE 754 binary64; `Rem` is `fmod` (the sign of `a`).
#[inline(always)]
pub(crate) fn float(op: FloatOp, a: f64, b: f64) -> f64 {
    match op {
        FloatOp::Add => a + b,
        FloatOp::Sub => a - b,
        FloatOp::Mul => a * b,
        FloatOp::Div => a / b,
        FloatOp::Rem => a % b,
        FloatOp::Pow => a.powf(b),
    }
}

/// `PartialOrd` order: on floats every compare with a NaN is false except
/// `Ne`, and `-0.0 == 0.0`.
#[inline(always)]
pub(crate) fn cmp<T: PartialOrd>(op: Cmp, a: T, b: T) -> bool {
    match op {
        Cmp::Lt => a < b,
        Cmp::Le => a <= b,
        Cmp::Gt => a > b,
        Cmp::Ge => a >= b,
        Cmp::Eq => a == b,
        Cmp::Ne => a != b,
    }
}

/// `bool` `&&` on two words (`true` is the word `1`).
#[inline(always)]
pub(crate) fn and(a: Value, b: Value) -> bool {
    a.as_bool() && b.as_bool()
}

/// `bool` `||` on two words (`true` is the word `1`).
#[inline(always)]
pub(crate) fn or(a: Value, b: Value) -> bool {
    a.as_bool() || b.as_bool()
}

#[cfg(test)]
mod tests {
    use common::int_arith::IntTrap;
    use common::{Instruction, Value, dense};

    use crate::Heap;

    const INTS: [i64; 12] = [i64::MIN, i64::MIN + 1, -64, -2, -1, 0, 1, 2, 63, 64, i64::MAX - 1, i64::MAX];
    const FLOATS: [f64; 10] = [f64::NEG_INFINITY, -1.5, -0.0, 0.0, 0.5, 1.0, 3.0, f64::INFINITY, f64::NAN, -f64::NAN];

    fn bits(r: Result<Value, IntTrap>) -> Result<u64, IntTrap> {
        r.map(|v| v.as_int() as u64)
    }

    /// Every 64-bit dense lane gives the fused (and so the plain) result
    /// bit for bit, traps included.
    #[test]
    fn dense_and_fused_bin_agree() {
        let heap = Heap::default();
        let ints = [
            (dense::IADD64, Instruction::ADD),
            (dense::ISUB64, Instruction::SUB),
            (dense::IMUL64, Instruction::MUL),
            (dense::IDIV64, Instruction::DIV),
            (dense::IREM64, Instruction::MOD),
            (dense::IAND64, Instruction::BITAND),
            (dense::IOR64, Instruction::BITOR),
            (dense::IXOR64, Instruction::XOR),
            (dense::ISHL64, Instruction::SHL),
            (dense::ISHR64, Instruction::SHR),
        ];
        for (kind, op) in ints {
            for a in INTS {
                for b in INTS {
                    let (a, b) = (Value::from(a), Value::from(b));
                    let d = bits(crate::dense::eval_bin(kind, a, b));
                    let f = bits(crate::fused::eval_bin(op as u8, a, b, &heap));
                    assert_eq!(d, f, "{op:?} {} {}", a.as_int(), b.as_int());
                }
            }
        }
        let floats = [
            (dense::FADD64, Instruction::ADDF),
            (dense::FSUB64, Instruction::SUBF),
            (dense::FMUL64, Instruction::MULF),
            (dense::FDIV64, Instruction::DIVF),
            (dense::FREM64, Instruction::MODF),
        ];
        for (kind, op) in floats {
            for a in FLOATS {
                for b in FLOATS {
                    let (a, b) = (Value::from(a), Value::from(b));
                    let d = bits(crate::dense::eval_bin(kind, a, b));
                    let f = bits(crate::fused::eval_bin(op as u8, a, b, &heap));
                    assert_eq!(d, f, "{op:?} {} {}", a.as_float(), b.as_float());
                }
            }
        }
    }

    /// Dense compares, fused compare-branches and the fused (plain) compare
    /// ops agree, NaN and `-0.0` included.
    #[test]
    fn compares_agree() {
        let heap = Heap::default();
        let preds = [dense::CMP_LT, dense::CMP_LE, dense::CMP_GT, dense::CMP_GE, dense::CMP_EQ, dense::CMP_NE];
        let int_ops = [Instruction::LE, Instruction::LEQ, Instruction::GT, Instruction::GEQ, Instruction::EQ, Instruction::NEQ];
        let float_ops = [Instruction::LEF, Instruction::LEQF, Instruction::GTF, Instruction::GEQF, Instruction::EQF, Instruction::NEQF];
        for (i, pred) in preds.into_iter().enumerate() {
            for a in INTS {
                for b in INTS {
                    let (a, b) = (Value::from(a), Value::from(b));
                    let d = crate::dense::eval_cmp(dense::pack_cmp(dense::CMP_I64, pred), a, b).as_int();
                    let op = int_ops[i] as u8;
                    assert_eq!(d, crate::fused::eval_bin(op, a, b, &heap).unwrap().as_int());
                    assert_eq!(d == 1, crate::fused::eval_cmp(op, a, b, &heap));
                }
            }
            for a in FLOATS {
                for b in FLOATS {
                    let (a, b) = (Value::from(a), Value::from(b));
                    let d = crate::dense::eval_cmp(dense::pack_cmp(dense::CMP_F64, pred), a, b).as_int();
                    let op = float_ops[i] as u8;
                    assert_eq!(d, crate::fused::eval_bin(op, a, b, &heap).unwrap().as_int());
                    assert_eq!(d == 1, crate::fused::eval_cmp(op, a, b, &heap));
                }
            }
        }
        let nan = Value::from(f64::NAN);
        assert!(!crate::fused::eval_cmp(Instruction::EQF as u8, nan, nan, &heap));
        assert!(crate::fused::eval_cmp(Instruction::NEQF as u8, nan, nan, &heap));
        assert!(crate::fused::eval_cmp(Instruction::EQF as u8, Value::from(-0.0), Value::from(0.0), &heap));
    }

    /// A fused bool / bitwise branch reads the word `eval_bin` would push,
    /// as `JMPT` does (`as_bool`).
    #[test]
    fn branch_reads_bin_result() {
        let heap = Heap::default();
        let ops = [Instruction::AND, Instruction::OR, Instruction::BITAND, Instruction::BITOR, Instruction::XOR];
        for op in ops {
            for a in [0i64, 1, 2, 3, -1] {
                for b in [0i64, 1, 2, 3, -1] {
                    let (a, b) = (Value::from(a), Value::from(b));
                    let pushed = crate::fused::eval_bin(op as u8, a, b, &heap).unwrap();
                    assert_eq!(pushed.as_bool(), crate::fused::eval_cmp(op as u8, a, b, &heap), "{op:?}");
                }
            }
        }
    }
}
