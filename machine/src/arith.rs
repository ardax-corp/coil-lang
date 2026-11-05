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

}
