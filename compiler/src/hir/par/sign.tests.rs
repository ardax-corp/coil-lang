//! Ranges of `int` values.

use super::*;
use crate::hir::build_module;
use crate::typechecking::infer::Checker;

fn iv(lo: i64, hi: i64) -> Interval {
    Interval { lo, hi }
}

/// What `f(args)` returns in program `src`.
fn result(src: &str, f: &str, args: &[Interval]) -> Interval {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    let sidecar = checker.typed_sidecar();
    let module = build_module(&checker, &sidecar, "", &ast);
    Signs::new(&module, &checker, "").result_of(f, args)
}

/// What `f` returns whatever it is passed.
fn any(src: &str, f: &str) -> Interval {
    result(src, f, &[])
}

#[test]
fn arithmetic_is_exact_and_cut_to_the_int_range() {
    assert_eq!(iv(1, 2).add(iv(10, 20)), Some(iv(11, 22)));
    assert_eq!(iv(1, 2).sub(iv(10, 20)), Some(iv(-19, -8)));
    assert_eq!(iv(-3, 2).mul(iv(-5, 4)), Some(iv(-12, 15)));
    assert_eq!(iv(-3, 2).square(), Some(iv(0, 9)));
    assert_eq!(iv(-3, -2).square(), Some(iv(4, 9)));
    assert_eq!(iv(i64::MAX - 1, i64::MAX).add(iv(0, 5)), Some(iv(i64::MAX - 1, i64::MAX)));
    // Always past the range: it traps, and there is no value.
    assert_eq!(iv(i64::MAX, i64::MAX).add(iv(1, 5)), None);
    assert_eq!(iv(i64::MIN, i64::MIN).neg(), None);
    assert_eq!(iv(i64::MIN, 5).neg(), Some(iv(-5, i64::MAX)));
    assert_eq!(iv(-4, 9).bit_not(), Some(iv(-10, 3)));
}

#[test]
fn division_and_remainder_follow_the_truncating_operators() {
    assert_eq!(iv(-7, 9).div(iv(2, 2)), Some(iv(-3, 4)));
    assert_eq!(iv(-7, 9).div(iv(-2, -2)), Some(iv(-4, 3)));
    assert_eq!(iv(0, 9).div(iv(1, 100)), Some(iv(0, 9)));
    assert_eq!(iv(5, 9).div(iv(0, 0)), None);
    // A divisor of either sign: no larger than the dividend.
    assert_eq!(iv(-7, 9).div(iv(-3, 3)), Some(iv(-9, 9)));
    // The remainder takes the dividend's sign.
    assert_eq!(iv(0, 1000).rem(iv(7, 7)), Some(iv(0, 6)));
    assert_eq!(iv(-1000, 0).rem(iv(7, 7)), Some(iv(-6, 0)));
    assert_eq!(iv(-1000, 1000).rem(iv(-7, 7)), Some(iv(-6, 6)));
    assert_eq!(iv(0, 3).rem(iv(7, 7)), Some(iv(0, 3)));
    assert_eq!(Interval::FULL.rem(iv(-1, -1)), Some(iv(0, 0)));
    assert_eq!(iv(1, 2).rem(iv(0, 0)), None);
}

#[test]
fn bitwise_operators_keep_a_sign_they_prove() {
    assert_eq!(Interval::FULL.bit_and(iv(0, 255)), Some(iv(0, 255)));
    assert_eq!(iv(0, 9).bit_and(iv(0, 255)), Some(iv(0, 9)));
    assert_eq!(iv(-9, -1).bit_and(iv(-5, -2)), Some(iv(i64::MIN, -1)));
    assert_eq!(Interval::FULL.bit_and(Interval::FULL), Some(Interval::FULL));
    assert_eq!(iv(1, 5).bit_or(iv(0, 9)), Some(iv(1, 15)));
    assert_eq!(iv(-3, -1).bit_or(Interval::FULL), Some(iv(i64::MIN, -1)));
    assert_eq!(iv(0, 5).bit_xor(iv(0, 9)), Some(iv(0, 15)));
    assert_eq!(iv(-5, -1).bit_xor(iv(-9, -2)), Some(Interval::NONNEG));
    assert_eq!(iv(-5, -1).bit_xor(iv(0, 9)), Some(iv(i64::MIN, -1)));
    assert_eq!(iv(0, i64::MAX).bit_or(iv(0, 1)), Some(iv(0, i64::MAX)));
}

#[test]
fn shifts_trap_on_a_count_outside_0_to_63() {
    assert_eq!(iv(0, 15).shl(iv(0, 12)), Some(iv(0, 15 << 12)));
    assert_eq!(iv(-2, 3).shl(iv(1, 2)), Some(iv(-8, 12)));
    // Bits may be shifted out: nothing is known.
    assert_eq!(iv(0, 1).shl(iv(0, 63)), Some(Interval::FULL));
    assert_eq!(iv(1, 1).shl(iv(64, 70)), None);
    assert_eq!(iv(-16, 64).shr(iv(2, 3)), Some(iv(-4, 16)));
    assert_eq!(iv(0, i64::MAX).shr(Interval::FULL), Some(iv(0, i64::MAX)));
    assert_eq!(iv(5, 9).shr(iv(-3, -1)), None);
}

#[test]
fn a_power_is_signed_by_its_base_or_an_even_exponent() {
    assert_eq!(iv(1, 9).pow(iv(0, 5)), Some(iv(1, i64::MAX)));
    assert_eq!(iv(0, 9).pow(iv(0, 5)), Some(Interval::NONNEG));
    assert_eq!(iv(-9, 9).pow(iv(2, 2)), Some(Interval::NONNEG));
    assert_eq!(iv(-9, 9).pow(iv(3, 3)), Some(Interval::FULL));
    assert_eq!(iv(2, 2).pow(iv(-5, -1)), None);
}

#[test]
fn a_literal_a_square_and_a_remainder_have_their_ranges() {
    let src = r#"
fn seven() -> int { return 7; }
fn sq(int x) -> int { return x * x; }
fn bucket(int x) -> int { return x % 10; }
fn low(int x) -> int { return x & 255; }
fn main() { return; }
"#;
    assert_eq!(any(src, "seven"), iv(7, 7));
    assert_eq!(any(src, "sq"), Interval::NONNEG);
    assert_eq!(any(src, "bucket"), iv(-9, 9));
    assert_eq!(result(src, "bucket", &[iv(0, 1000)]), iv(0, 9));
    assert_eq!(result(src, "bucket", &[iv(3, 4)]), iv(0, 4));
    assert_eq!(any(src, "low"), iv(0, 255));
}

#[test]
fn a_branch_narrows_a_local_nothing_writes() {
    let src = r#"
fn abs(int x) -> int {
    if x < 0 {
        return -x;
    }
    return x;
}
fn clamp(int x) -> int {
    if x < 3 { return 3; }
    if x > 9 { return 9; }
    return x;
}
fn sign(int x) -> int {
    if x > 0 { return 1; } else if x == 0 { return 0; }
    return 0 - 1;
}
fn both(int x, int y) -> int {
    if x >= 0 && y >= 1 { return x * y; }
    return 0;
}
fn either(int x, int y) -> int {
    if x < 0 || y < 0 { return 0; }
    return x + y;
}
fn main() { return; }
"#;
    assert_eq!(any(src, "abs"), Interval::NONNEG);
    assert_eq!(any(src, "clamp"), iv(3, 9));
    assert_eq!(any(src, "sign"), iv(-1, 1));
    assert_eq!(any(src, "both"), Interval::NONNEG);
    assert_eq!(any(src, "either"), Interval::NONNEG);
}

#[test]
fn a_branch_does_not_narrow_a_local_the_body_writes() {
    let src = r#"
fn drift(int x) -> int {
    if x < 0 {
        return 0;
    }
    x = x - 100;
    return x;
}
fn swap(int x) -> int {
    let y = x;
    if y >= 0 {
        y = 0 - y;
        return y;
    }
    return 0;
}
fn main() { return; }
"#;
    assert_eq!(any(src, "drift"), Interval::FULL);
    assert_eq!(any(src, "swap"), Interval::FULL);
}

#[test]
fn a_local_written_in_a_loop_holds_every_value_written() {
    let src = r#"
fn sum_sq(int n) -> int {
    let s = 0;
    let i = 0;
    while i < n {
        s = s + i * i;
        i = i + 1;
    }
    return s;
}
fn count_down(int n) -> int {
    let s = 0;
    let i = 0;
    while i < n {
        s = s - 2;
        i = i + 1;
    }
    return s;
}
fn wobble(int n) -> int {
    let s = 1;
    for i in 0..n {
        s = s * (0 - 2);
    }
    return s;
}
fn range_local(int n) -> int {
    let r = 0..n;
    let acc = 0;
    for x in r {
        acc = acc + x;
    }
    return acc;
}
fn main() { return; }
"#;
    assert_eq!(any(src, "sum_sq"), Interval::NONNEG);
    assert_eq!(any(src, "count_down"), iv(i64::MIN, 0));
    assert_eq!(any(src, "wobble"), Interval::FULL);
    assert_eq!(any(src, "range_local"), Interval::NONNEG);
}

#[test]
fn a_match_on_literals_joins_its_arms() {
    let src = r#"
fn cost(int op) -> int {
    return match op {
        0 => 3,
        1 => 5,
        default => 1,
    };
}
fn back(int op) -> int {
    return match op {
        0 => 0 - 3,
        other => other,
    };
}
fn main() { return; }
"#;
    assert_eq!(any(src, "cost"), iv(1, 5));
    assert_eq!(any(src, "back"), Interval::FULL);
    assert_eq!(result(src, "back", &[iv(1, 4)]), iv(1, 4));
}

#[test]
fn a_call_is_what_its_body_returns_for_those_arguments() {
    let src = r#"
fn weight(int i) -> int { return i % 7; }
fn twice(int i) -> int { return weight(i) + weight(i + 1); }
fn down(int n) -> int {
    if n <= 0 { return 0; }
    return 1 + down(n - 1);
}
fn main() { return; }
"#;
    assert_eq!(result(src, "twice", &[iv(0, 100)]), iv(0, 12));
    assert_eq!(any(src, "twice"), iv(-12, 12));
    // A function that calls itself is unknown where it does.
    assert_eq!(any(src, "down"), iv(i64::MIN + 1, i64::MAX));
}

#[test]
fn a_length_and_a_byte_are_not_negative() {
    let src = r#"
fn count(Vec<int> xs) -> int { return xs.len(); }
fn first(Vec<byte> xs) -> int { return xs[0] as int; }
fn elem(Vec<int> xs) -> int { return xs[0]; }
fn main() { return; }
"#;
    assert_eq!(any(src, "count"), Interval::NONNEG);
    assert_eq!(any(src, "first"), iv(0, 255));
    assert_eq!(any(src, "elem"), Interval::FULL);
}

#[test]
fn a_const_and_a_readonly_table_are_known_and_a_static_is_not() {
    let src = r#"
const THRICE: int = 3;
const STARTED: int = seven();
const WEIGHTS = readonly [1, 2, 3, 4];
const LOOSE = [1, 2, 3, 4];
static let COUNT: int = 41;
fn seven() -> int { return 7; }
fn tripled(int i) -> int { return i * THRICE; }
// A `const` set at startup reads `0` in code that runs before that.
fn started() -> int { return STARTED; }
fn weighted(int i) -> int { return WEIGHTS[i % 4]; }
fn loose(int i) -> int { return LOOSE[i % 4]; }
fn counted() -> int { return COUNT; }
fn local_table(int i) -> int {
    const BOX = readonly [12, 5, 6, 11];
    return BOX[i & 3];
}
fn main() { return; }
"#;
    assert_eq!(result(src, "tripled", &[iv(0, 10)]), iv(0, 30));
    assert_eq!(any(src, "started"), iv(0, 7));
    assert_eq!(any(src, "weighted"), iv(0, 4));
    // Its elements may be written through the binding.
    assert_eq!(any(src, "loose"), Interval::FULL);
    assert_eq!(any(src, "counted"), Interval::FULL);
    assert_eq!(any(src, "local_table"), iv(5, 12));
}

#[test]
fn a_check_that_panics_holds_after_it() {
    let src = r#"
fn guarded(int k) -> int {
    if k < 1 {
        panic "k";
    }
    return k;
}
fn main() { return; }
"#;
    assert_eq!(any(src, "guarded"), iv(1, i64::MAX));
}
