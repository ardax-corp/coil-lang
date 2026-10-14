// A `return` of a call to the body itself or to a sibling in its recursion
// cycle takes the caller's frame (`TailCall`) on direct MIR, as on the HIR
// stack lowering: a deep tail recursion runs in one frame.
use tests::assert;

fn sum_to(int n, int acc) -> int {
    if n == 0 {
        return acc;
    }
    return sum_to(n - 1, acc + n);
}

fn even(int n) -> bool {
    if n == 0 {
        return true;
    }
    return odd(n - 1);
}

fn odd(int n) -> bool {
    if n == 0 {
        return false;
    }
    return even(n - 1);
}

fn bounce(Option<int> o, int n) -> int {
    if n == 0 {
        return 7;
    }
    return match o {
        Option::None => bounce(Option::Some(0), n - 1),
        Option::Some(_) => bounce(Option::None, n - 1),
    };
}

test("self tail calls run in one frame") {
    assert!(sum_to(3000000, 0) == 4500001500000);
}

test("sibling tail calls run in one frame") {
    assert!(odd(3000001));
}

test("tail calls in match arms run in one frame") {
    assert!(bounce(Option::None, 3000000) == 7);
}
