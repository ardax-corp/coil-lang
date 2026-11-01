// A trait-instance method returns its `Option` boxed; several early
// `return None;` ahead of a final `return Some(..)` all build that box
// (coil-lang#1271).
use tests::assert;

const INT_MIN = 0 - 9223372036854775807 - 1;

trait Checked<S> {
    fn checked_div(S a, S b) -> Option<S> {}
}

impl Checked for int {
    pub fn checked_div(int a, int b) -> Option<int> {
        if b == 0 {
            return None;
        }
        if a == INT_MIN && b == 0 - 1 {
            return None;
        }
        return Some(a / b);
    }
}

trait Pick<S> {
    fn pick(S a) -> Option<int> {}
}

impl Pick for int {
    pub fn pick(int a) -> Option<int> {
        if a == 0 {
            return None;
        }
        if a == 1 {
            return None;
        }
        return Some(a);
    }
}

test("checked_div with two early None returns") {
    assert!((7.checked_div(2) ?? 0) == 3);
    assert!((7.checked_div(0) ?? 42) == 42);
    assert!((INT_MIN.checked_div(0 - 1) ?? 42) == 42);
    assert!((INT_MIN.checked_div(1) ?? 0) == INT_MIN);
}

test("concrete Option return with two early None returns") {
    assert!((7.pick() ?? 0) == 7);
    assert!((0.pick() ?? 42) == 42);
    assert!((1.pick() ?? 42) == 42);
}
