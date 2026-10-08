// Nested match hot loop: many rows share an outer tag, so a decision tree
// tests the tag once and then only the sub-patterns of that tag's rows.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

enum Op {
    Add(int, int),
    Mul(int, int),
    Neg(Option<int>),
    Nop,
}

fn eval(Op op) -> int {
    return match op {
        Op::Add(0, b) => b,
        Op::Mul(1, b) => b,
        Op::Add(a, 0) => a,
        Op::Mul(0, _) => 0,
        Op::Add(1, b) => b + 1,
        Op::Mul(a, 1) => a,
        Op::Neg(Option::Some(0)) => 0,
        Op::Add(a, b) => a + b,
        Op::Mul(a, b) => a * b,
        Op::Neg(Option::Some(n)) => 0 - n,
        default => 1,
    };
}

fn run() -> int {
    let ops = [
        Op::Add(2, 3),
        Op::Mul(4, 5),
        Op::Neg(Option::Some(6)),
        Op::Nop,
        Op::Add(0, 7),
        Op::Mul(1, 8),
        Op::Add(9, 0),
        Op::Neg(Option::None),
    ];
    let acc = 0;
    let i = 0;
    while i < 250000 {
        for op in ops {
            acc = acc + eval(op);
        }
        i = i + 1;
    }
    return acc;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", run())));
}
