// A miss on a boxed enum's `JumpIfMatch` leaves the scrutinee on the operand
// stack. When the arm it falls to is also reached by a failed sub-pattern
// test (`Op::Mul(a, 1)` matched `Mul` but not the `1`), that path has no
// scrutinee left, so the miss must take it off before the join, or a loop
// running the match grows the stack until it corrupts memory (#1780).
use tests::assert;

enum Op {
    Add(int, int),
    Mul(int, int),
    Neg(Option<int>),
    Nop,
}

fn eval(Op op) -> int {
    return match op {
        Op::Mul(a, 1) => a,
        default => 1,
    };
}

fn nested(Op op) -> int {
    return match op {
        Op::Add(0, b) => b,
        Op::Mul(1, b) => b,
        Op::Add(a, 0) => a,
        Op::Mul(a, 1) => a,
        Op::Neg(Option::Some(0)) => 0,
        Op::Add(a, b) => a + b,
        Op::Mul(a, b) => a * b,
        Op::Neg(Option::Some(n)) => 0 - n,
        default => 1,
    };
}

fn run(Vec<Op> ops, int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        for op in ops {
            acc = acc + eval(op);
        }
        i = i + 1;
    }
    return acc;
}

fn run_nested(Vec<Op> ops, int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        for op in ops {
            acc = acc + nested(op);
        }
        i = i + 1;
    }
    return acc;
}

test("a miss joining a failed sub-pattern leaves the stack as it was") {
    assert!(run([Op::Neg(Option::Some(6))], 200000) == 200000);
    assert!(run([Op::Mul(4, 1), Op::Mul(4, 2), Op::Nop], 100000) == 600000);
    let ops: Vec<Op> = [
        Op::Add(2, 3),
        Op::Mul(4, 5),
        Op::Neg(Option::Some(6)),
        Op::Nop,
        Op::Add(0, 7),
        Op::Mul(1, 8),
        Op::Add(9, 0),
        Op::Neg(Option::None),
    ];
    // 5 + 20 - 6 + 1 + 7 + 8 + 9 + 1 = 45 per pass.
    assert!(run_nested(ops, 100000) == 4500000);
}
