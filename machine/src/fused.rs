//! Fused-op jump tables: packed inner opcodes are `u8` ISA ids, same decode.
//!
//! rustc 1.98 has no stable guaranteed tail calls (`become` is nightly), so
//! token-threading the main loop is out. These tables replace the second
//! `match Instruction::from(op)` inside fused handlers with a 256-entry
//! function pointer table (same operands, same results).

use common::{Instruction, Value};

use crate::Heap;

type BinFn = fn(Value, Value, &Heap) -> Value;
type CmpFn = fn(Value, Value, &Heap) -> bool;
type F64BinFn = fn(f64, f64) -> f64;
type F64CmpFn = fn(f64, f64) -> bool;

#[inline(always)]
pub(crate) fn eval_bin(op: u8, lhs: Value, rhs: Value, heap: &Heap) -> Value {
    // `op` is a packed Instruction discriminant (0..=255).
    BIN[op as usize](lhs, rhs, heap)
}

#[inline(always)]
pub(crate) fn eval_cmp(op: u8, lhs: Value, rhs: Value, heap: &Heap) -> bool {
    CMP[op as usize](lhs, rhs, heap)
}

#[inline(always)]
pub(crate) fn eval_f64_bin(op: u8, lhs: f64, rhs: f64) -> f64 {
    F64_BIN[op as usize](lhs, rhs)
}

#[inline(always)]
pub(crate) fn eval_f64_cmp(op: u8, lhs: f64, rhs: f64) -> bool {
    F64_CMP[op as usize](lhs, rhs)
}

#[inline(always)]
fn bin_default(_: Value, _: Value, _: &Heap) -> Value {
    Value::default()
}

#[inline(always)]
fn cmp_default(_: Value, _: Value, _: &Heap) -> bool {
    false
}

#[inline(always)]
fn f64_bin_default(_: f64, _: f64) -> f64 {
    f64::NAN
}

#[inline(always)]
fn f64_cmp_default(_: f64, _: f64) -> bool {
    false
}

macro_rules! bin_int {
    ($name:ident, $op:tt) => {
        #[inline(always)]
        fn $name(a: Value, b: Value, _: &Heap) -> Value {
            Value::from(a.as_int() $op b.as_int())
        }
    };
}

macro_rules! bin_int_cmp {
    ($name:ident, $op:tt) => {
        #[inline(always)]
        fn $name(a: Value, b: Value, _: &Heap) -> Value {
            Value::from((a.as_int() $op b.as_int()) as i64)
        }
    };
}

macro_rules! bin_float {
    ($name:ident, $op:tt) => {
        #[inline(always)]
        fn $name(a: Value, b: Value, _: &Heap) -> Value {
            Value::from(a.as_float() $op b.as_float())
        }
    };
}

macro_rules! bin_float_cmp {
    ($name:ident, $op:tt) => {
        #[inline(always)]
        fn $name(a: Value, b: Value, _: &Heap) -> Value {
            Value::from((a.as_float() $op b.as_float()) as i64)
        }
    };
}

macro_rules! cmp_int {
    ($name:ident, $op:tt) => {
        #[inline(always)]
        fn $name(a: Value, b: Value, _: &Heap) -> bool {
            a.as_int() $op b.as_int()
        }
    };
}

macro_rules! cmp_float {
    ($name:ident, $op:tt) => {
        #[inline(always)]
        fn $name(a: Value, b: Value, _: &Heap) -> bool {
            a.as_float() $op b.as_float()
        }
    };
}

macro_rules! f64_bin {
    ($name:ident, $op:tt) => {
        #[inline(always)]
        fn $name(a: f64, b: f64) -> f64 {
            a $op b
        }
    };
}

macro_rules! f64_cmp {
    ($name:ident, $op:tt) => {
        #[inline(always)]
        fn $name(a: f64, b: f64) -> bool {
            a $op b
        }
    };
}

bin_int!(bin_add, +);
bin_int!(bin_sub, -);
bin_int!(bin_mul, *);
bin_int!(bin_div, /);
bin_int!(bin_mod, %);
bin_int!(bin_bitand, &);
bin_int!(bin_bitor, |);
bin_int!(bin_xor, ^);
bin_int!(bin_shl, <<);
bin_int!(bin_shr, >>);
bin_int_cmp!(bin_le, <);
bin_int_cmp!(bin_leq, <=);
bin_int_cmp!(bin_gt, >);
bin_int_cmp!(bin_geq, >=);
bin_float!(bin_addf, +);
bin_float!(bin_subf, -);
bin_float!(bin_mulf, *);
bin_float!(bin_divf, /);
bin_float!(bin_modf, %);
bin_float_cmp!(bin_lef, <);
bin_float_cmp!(bin_leqf, <=);
bin_float_cmp!(bin_gtf, >);
bin_float_cmp!(bin_geqf, >=);

cmp_int!(cmp_le, <);
cmp_int!(cmp_leq, <=);
cmp_int!(cmp_gt, >);
cmp_int!(cmp_geq, >=);
cmp_float!(cmp_lef, <);
cmp_float!(cmp_leqf, <=);
cmp_float!(cmp_gtf, >);
cmp_float!(cmp_geqf, >=);
f64_bin!(f64_add, +);
f64_bin!(f64_sub, -);
f64_bin!(f64_mul, *);
f64_bin!(f64_div, /);
f64_bin!(f64_mod, %);
f64_cmp!(f64_le, <);
f64_cmp!(f64_leq, <=);
f64_cmp!(f64_gt, >);
f64_cmp!(f64_geq, >=);

#[inline(always)]
fn bin_pow(a: Value, b: Value, _: &Heap) -> Value {
    let exp = b.as_int().max(0) as u32;
    Value::from(a.as_int().pow(exp))
}

#[inline(always)]
fn bin_powf(a: Value, b: Value, _: &Heap) -> Value {
    Value::from(a.as_float().powf(b.as_float()))
}

#[inline(always)]
fn bin_and(a: Value, b: Value, _: &Heap) -> Value {
    Value::from(a.as_bool() && b.as_bool())
}

#[inline(always)]
fn bin_or(a: Value, b: Value, _: &Heap) -> Value {
    Value::from(a.as_bool() || b.as_bool())
}

#[inline(always)]
fn bin_eq(a: Value, b: Value, heap: &Heap) -> Value {
    Value::from(crate::value_eq::values_eq(heap, a, b) as i64)
}

#[inline(always)]
fn bin_neq(a: Value, b: Value, heap: &Heap) -> Value {
    Value::from((!crate::value_eq::values_eq(heap, a, b)) as i64)
}

#[inline(always)]
fn cmp_eq(a: Value, b: Value, heap: &Heap) -> bool {
    crate::value_eq::values_eq(heap, a, b)
}

#[inline(always)]
fn cmp_neq(a: Value, b: Value, heap: &Heap) -> bool {
    !crate::value_eq::values_eq(heap, a, b)
}

#[inline(always)]
fn cmp_and(a: Value, b: Value, _: &Heap) -> bool {
    a.as_bool() && b.as_bool()
}

#[inline(always)]
fn cmp_or(a: Value, b: Value, _: &Heap) -> bool {
    a.as_bool() || b.as_bool()
}

#[inline(always)]
fn cmp_bitand(a: Value, b: Value, _: &Heap) -> bool {
    Value::from(a.as_int() & b.as_int()).as_bool()
}

#[inline(always)]
fn cmp_bitor(a: Value, b: Value, _: &Heap) -> bool {
    Value::from(a.as_int() | b.as_int()).as_bool()
}

#[inline(always)]
fn cmp_xor(a: Value, b: Value, _: &Heap) -> bool {
    Value::from(a.as_int() ^ b.as_int()).as_bool()
}

const fn slot(op: Instruction) -> usize {
    op as u8 as usize
}

const fn bin_table() -> [BinFn; 256] {
    let mut t = [bin_default as BinFn; 256];
    t[slot(Instruction::ADD)] = bin_add;
    t[slot(Instruction::SUB)] = bin_sub;
    t[slot(Instruction::MUL)] = bin_mul;
    t[slot(Instruction::DIV)] = bin_div;
    t[slot(Instruction::MOD)] = bin_mod;
    t[slot(Instruction::Pow)] = bin_pow;
    t[slot(Instruction::BITAND)] = bin_bitand;
    t[slot(Instruction::BITOR)] = bin_bitor;
    t[slot(Instruction::SHL)] = bin_shl;
    t[slot(Instruction::SHR)] = bin_shr;
    t[slot(Instruction::XOR)] = bin_xor;
    t[slot(Instruction::AND)] = bin_and;
    t[slot(Instruction::OR)] = bin_or;
    t[slot(Instruction::LE)] = bin_le;
    t[slot(Instruction::LEQ)] = bin_leq;
    t[slot(Instruction::GT)] = bin_gt;
    t[slot(Instruction::GEQ)] = bin_geq;
    t[slot(Instruction::EQ)] = bin_eq;
    t[slot(Instruction::NEQ)] = bin_neq;
    t[slot(Instruction::ADDF)] = bin_addf;
    t[slot(Instruction::SUBF)] = bin_subf;
    t[slot(Instruction::MULF)] = bin_mulf;
    t[slot(Instruction::DIVF)] = bin_divf;
    t[slot(Instruction::MODF)] = bin_modf;
    t[slot(Instruction::LEF)] = bin_lef;
    t[slot(Instruction::LEQF)] = bin_leqf;
    t[slot(Instruction::GTF)] = bin_gtf;
    t[slot(Instruction::GEQF)] = bin_geqf;
    t[slot(Instruction::PowF)] = bin_powf;
    t
}

const fn cmp_table() -> [CmpFn; 256] {
    let mut t = [cmp_default as CmpFn; 256];
    t[slot(Instruction::LE)] = cmp_le;
    t[slot(Instruction::LEQ)] = cmp_leq;
    t[slot(Instruction::GT)] = cmp_gt;
    t[slot(Instruction::GEQ)] = cmp_geq;
    t[slot(Instruction::EQ)] = cmp_eq;
    t[slot(Instruction::NEQ)] = cmp_neq;
    t[slot(Instruction::LEF)] = cmp_lef;
    t[slot(Instruction::LEQF)] = cmp_leqf;
    t[slot(Instruction::GTF)] = cmp_gtf;
    t[slot(Instruction::GEQF)] = cmp_geqf;
    t[slot(Instruction::AND)] = cmp_and;
    t[slot(Instruction::OR)] = cmp_or;
    t[slot(Instruction::BITAND)] = cmp_bitand;
    t[slot(Instruction::BITOR)] = cmp_bitor;
    t[slot(Instruction::XOR)] = cmp_xor;
    t
}

const fn f64_bin_table() -> [F64BinFn; 256] {
    let mut t = [f64_bin_default as F64BinFn; 256];
    t[slot(Instruction::ADDF)] = f64_add;
    t[slot(Instruction::SUBF)] = f64_sub;
    t[slot(Instruction::MULF)] = f64_mul;
    t[slot(Instruction::DIVF)] = f64_div;
    t[slot(Instruction::MODF)] = f64_mod;
    t
}

const fn f64_cmp_table() -> [F64CmpFn; 256] {
    let mut t = [f64_cmp_default as F64CmpFn; 256];
    t[slot(Instruction::LEF)] = f64_le;
    t[slot(Instruction::LEQF)] = f64_leq;
    t[slot(Instruction::GTF)] = f64_gt;
    t[slot(Instruction::GEQF)] = f64_geq;
    t
}

static BIN: [BinFn; 256] = bin_table();
static CMP: [CmpFn; 256] = cmp_table();
static F64_BIN: [F64BinFn; 256] = f64_bin_table();
static F64_CMP: [F64CmpFn; 256] = f64_cmp_table();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fused_bin_table_matches_int_arith() {
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
    fn fused_f64_tables_cover_mandelbrot_ops() {
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
        let z = eval_bin(Instruction::HALT as u8, Value::from(1i64), Value::from(2i64), &heap);
        assert_eq!(z.as_int(), 0);
        assert!(!eval_cmp(
            Instruction::HALT as u8,
            Value::from(1i64),
            Value::from(2i64),
            &heap
        ));
    }
}
