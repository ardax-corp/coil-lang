//! Coil `int` arithmetic: every operation either gives the exact result or
//! traps. The VM raises the trap as a Coil panic; a constant folder leaves
//! the operation for run time instead.
//!
//! `MIN % -1` is `0` (the exact remainder), not a trap.

/// Why an int operation has no result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntTrap {
    Overflow,
    DivByZero,
    NegativeExponent,
}

impl IntTrap {
    /// The panic message.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            IntTrap::Overflow => "integer overflow",
            IntTrap::DivByZero => "division by zero",
            IntTrap::NegativeExponent => "negative exponent",
        }
    }
}

pub type IntResult = Result<i64, IntTrap>;

#[inline(always)]
pub fn add(a: i64, b: i64) -> IntResult {
    a.checked_add(b).ok_or(IntTrap::Overflow)
}

#[inline(always)]
pub fn sub(a: i64, b: i64) -> IntResult {
    a.checked_sub(b).ok_or(IntTrap::Overflow)
}

#[inline(always)]
pub fn mul(a: i64, b: i64) -> IntResult {
    a.checked_mul(b).ok_or(IntTrap::Overflow)
}

#[inline(always)]
pub fn neg(a: i64) -> IntResult {
    a.checked_neg().ok_or(IntTrap::Overflow)
}

#[inline(always)]
pub fn div(a: i64, b: i64) -> IntResult {
    if b == 0 {
        return Err(IntTrap::DivByZero);
    }
    a.checked_div(b).ok_or(IntTrap::Overflow)
}

#[inline(always)]
pub fn rem(a: i64, b: i64) -> IntResult {
    if b == 0 {
        return Err(IntTrap::DivByZero);
    }
    Ok(a.wrapping_rem(b))
}

/// `a ** b`.
pub fn pow(a: i64, b: i64) -> IntResult {
    if b < 0 {
        return Err(IntTrap::NegativeExponent);
    }
    match a {
        0 => return Ok(i64::from(b == 0)),
        1 => return Ok(1),
        -1 => return Ok(if b % 2 == 0 { 1 } else { -1 }),
        _ => {}
    }
    let b = u32::try_from(b).map_err(|_| IntTrap::Overflow)?;
    a.checked_pow(b).ok_or(IntTrap::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_or_trap() {
        assert_eq!(add(i64::MAX, 1), Err(IntTrap::Overflow));
        assert_eq!(add(i64::MAX - 1, 1), Ok(i64::MAX));
        assert_eq!(sub(i64::MIN, 1), Err(IntTrap::Overflow));
        assert_eq!(mul(1 << 32, 1 << 31), Err(IntTrap::Overflow));
        assert_eq!(mul(-(1 << 32), 1 << 31), Ok(i64::MIN));
        assert_eq!(neg(i64::MIN), Err(IntTrap::Overflow));
        assert_eq!(div(7, 0), Err(IntTrap::DivByZero));
        assert_eq!(div(i64::MIN, -1), Err(IntTrap::Overflow));
        assert_eq!(div(-7, 2), Ok(-3));
        assert_eq!(rem(i64::MIN, -1), Ok(0));
        assert_eq!(rem(-7, 2), Ok(-1));
        assert_eq!(rem(1, 0), Err(IntTrap::DivByZero));
        assert_eq!(pow(2, 62), Ok(1 << 62));
        assert_eq!(pow(2, 63), Err(IntTrap::Overflow));
        assert_eq!(pow(-2, 63), Ok(i64::MIN));
        assert_eq!(pow(-1, i64::MAX), Ok(-1));
        assert_eq!(pow(0, 0), Ok(1));
        assert_eq!(pow(3, -1), Err(IntTrap::NegativeExponent));
    }
}
