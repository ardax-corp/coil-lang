// An int `x != 0` branch emitted from MIR as stack IL must compare the
// word with zero: `JMPF` tests a bool word, so branching on `x` itself is
// only right when `x` is 0 or 1 (as `i & 1` is).
use tests::assert;

fn guarded_div(int d) -> bool {
    return d != 0 && 10 / d == 5;
}

fn odd(int i) -> bool {
    return (i & 1) != 0;
}

test("a nonzero int other than one takes the true branch") {
    assert!(!guarded_div(0));
    assert!(guarded_div(2));
    assert!(!guarded_div(3));
    assert!(guarded_div(-2) == false);
}

test("a masked low bit still branches") {
    assert!(odd(3));
    assert!(!odd(4));
}
