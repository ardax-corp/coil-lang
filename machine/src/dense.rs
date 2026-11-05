//! Typed dense numeric ops (COI-268). Operate on raw slot bits; no boxing.

use common::int_arith::{self, IntTrap};
use common::{Value, dense};

use crate::arith::{self, };


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unary_not_matches_log_not_truthiness() {
        // VM LogNot is `!(as_int() != 0)`. Dense UNARY_NOT used to use
        // `as_bool` (`raw as u8 == 1`), so a heap pointer looked like None.
        let ptr = Value::from(0x1000i64);
        assert_eq!(eval_unary(dense::UNARY_NOT, Value::from(0i64)).unwrap().as_int(), 1);
        assert_eq!(eval_unary(dense::UNARY_NOT, Value::from(1i64)).unwrap().as_int(), 0);
        assert_eq!(eval_unary(dense::UNARY_NOT, Value::from(42i64)).unwrap().as_int(), 0);
        assert_eq!(eval_unary(dense::UNARY_NOT, ptr).unwrap().as_int(), 0);
        assert_eq!(eval_unary(dense::UNARY_NOT, Value::from(true)).unwrap().as_int(), 0);
        assert_eq!(eval_unary(dense::UNARY_NOT, Value::from(false)).unwrap().as_int(), 1);
    }
}
