// A bare name as a whole arm matches anything left and binds the whole
// scrutinee (an int or a user enum); a name in a payload slot binds just
// that slot.
use tests::assert;

enum Op {
    Neg(int),
    Add(int, int),
    Nop,
}

fn classify(int n) -> int {
    return match n {
        0 => 100,
        other => other * 2,
    };
}

fn eval(Op op) -> int {
    return match op {
        Op::Neg(v) => -v,
        whole => match whole {
            Op::Add(a, b) => a + b,
            default => 0,
        },
    };
}

test("a binding arm on an int") {
    assert!(classify(0) == 100);
    assert!(classify(4) == 8);
    assert!(classify(-3) == -6);
}

test("a binding arm on an enum binds the whole value") {
    assert!(eval(Op::Neg(3)) == -3);
    assert!(eval(Op::Add(2, 5)) == 7);
    assert!(eval(Op::Nop) == 0);
}
