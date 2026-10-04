// Operands that lower with their own branches (`match`, `??`) or through a
// tiny-inlined call keep source order next to a value already on the stack.

fn twice(int a) -> int {
    return a * 2;
}

fn sub(int a, int b) -> int {
    return a - b;
}

class Cell {
    pub v: int,
}

test("binary operand after a local") {
    let o = Option::Some(7);
    let s = 100;
    assert(s - (o ?? 1) == 93)?;
    let t = s - match o {
        Option::Some(v) => v,
        Option::None => 1,
    };
    assert(t == 93)?;
}

test("call argument after a local") {
    let o = Option::Some(7);
    let s = 100;
    assert(sub(s, o ?? 1) == 93)?;
}

test("compound assign with a branching rhs") {
    let o = Option::Some(7);
    let s = 100;
    s -= o ?? 1;
    assert(s == 93)?;
    let a = [100, 20];
    a[0] -= match o {
        Option::Some(v) => v,
        Option::None => 1,
    };
    assert(a[0] == 93)?;
    let c = new Cell(100);
    c.v -= o ?? 1;
    assert(c.v == 93)?;
}

test("compound assign with an inlined call in a loop") {
    let s = 0;
    let i = 0;
    while i < 10 {
        if i % 2 == 0 {
            s += twice(i);
        } else if i > 5 {
            break;
        }
        i++;
    }
    assert(s == 24)?;
}
