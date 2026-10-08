// A nested match tests the outer tag once: rows that share an outer variant
// test only their sub-patterns on the payload that variant unpacked, and a
// tag whose rows all miss falls into the catch-all.

enum Op {
    Add(int, int, int),
    Neg(Option<int>),
    Nop,
}

fn code(Op op) -> int {
    return match op {
        Op::Add(0, 0, z) => z,
        Op::Neg(Option::Some(1)) => -1,
        Op::Add(1, y, 2) => y * 10,
        Op::Neg(Option::None) => -2,
        Op::Add(x, 3, _) => x + 300,
        other => match other {
            Op::Nop => 0,
            default => 99,
        },
    };
}

fn exhaustive(Op op) -> int {
    return match op {
        Op::Add(0, _, _) => 1,
        Op::Add(_, 0, _) => 2,
        Op::Add(a, b, c) => a + b + c,
        Op::Neg(Option::Some(n)) => n,
        Op::Neg(Option::None) => -5,
        Op::Nop => 7,
    };
}

fn count([Op] ops) -> int {
    let acc = 0;
    for op in ops {
        match op {
            Op::Add(1, _, _) => {
                acc = acc + 1;
            },
            Op::Add(_, 1, _) => {
                acc = acc + 10;
            },
            default => {
                acc = acc + 100;
            },
        }
    }
    return acc;
}

test("rows of one tag test only their sub-patterns") {
    assert(code(Op::Add(0, 0, 9)) == 9, "add zero")?;
    assert(code(Op::Add(1, 4, 2)) == 40, "add one")?;
    assert(code(Op::Add(5, 3, 8)) == 305, "add three")?;
    assert(code(Op::Neg(Option::Some(1))) == -1, "neg one")?;
    assert(code(Op::Neg(Option::None)) == -2, "neg none")?;
}

test("a tag whose rows all miss takes the catch-all") {
    assert(code(Op::Add(1, 4, 5)) == 99, "add miss")?;
    assert(code(Op::Neg(Option::Some(2))) == 99, "neg miss")?;
    assert(code(Op::Nop) == 0, "nop")?;
}

test("an exhaustive match needs no catch-all") {
    assert(exhaustive(Op::Add(0, 5, 5)) == 1, "first")?;
    assert(exhaustive(Op::Add(4, 0, 5)) == 2, "second")?;
    assert(exhaustive(Op::Add(1, 2, 3)) == 6, "bind")?;
    assert(exhaustive(Op::Neg(Option::Some(4))) == 4, "some")?;
    assert(exhaustive(Op::Neg(Option::None)) == -5, "none")?;
    assert(exhaustive(Op::Nop) == 7, "nop")?;
}

test("a statement match in a loop") {
    let ops = [Op::Add(1, 1, 1), Op::Add(2, 1, 0), Op::Nop, Op::Add(3, 3, 3)];
    assert(count(ops) == 211, "count")?;
}
