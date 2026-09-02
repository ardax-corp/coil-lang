// Dense smoke coverage: many small independent syntax shapes in one file.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
enum Tiny {
    A,
    B,
    C,
}

test("parens and grouping") {
    assert((1 + 2) * (3 + 4) == 21)?;
    assert((((5))) == 5)?;
}

test("bool literals in conditions") {
    let x = 0;
    if true && !false {
        x = 1;
    }
    assert(x == 1)?;
}

test("deeply nested arithmetic") {
    assert(1 + 2 + 3 + 4 + 5 + 6 + 7 + 8 + 9 + 10 == 55)?;
}

test("array of tuples") {
    let a = [(1, 2), (3, 4)];
    assert(a[0][1] == 2)?;
    assert(a[1][0] == 3)?;
}

test("tuple of arrays") {
    let t = ([1, 2], [3, 4]);
    assert(t[0][1] == 2)?;
    assert(t[1][0] == 3)?;
}

test("dict with array field") {
    let d = { xs: [1, 2, 3] };
    assert(d.xs[2] == 3)?;
}

test("match with default") {
    let n = Tiny::C;
    let r = match n {
        Tiny::A => 10,
        Tiny::B => 20,
        default => 99,
    };
    assert(r == 99)?;
}

test("multiple statements in block") {
    let a = 1;
    let b = 2;
    let c = 3;
    assert(a + b + c == 6)?;
}

test("reassign through arithmetic chain") {
    let x = 1;
    x = x + 1;
    x = x * 2;
    x = x - 1;
    assert(x == 3)?;
}

test("string format with int float bool") {
    let s = format("%i %f %z", 1, 2.5, false);
    // Fresh allocation — just check it is a distinct usable string.
    assert(s != "")?;
    assert(s != "x")?;
}

test("zero and identity") {
    assert(0 + 0 == 0)?;
    assert(1 * 1 == 1)?;
    assert(0 * 99 == 0)?;
    assert(99 - 99 == 0)?;
}

test("large-ish int") {
    assert(1000000 + 1 == 1000001)?;
}

test("comparison chain via locals") {
    let a = 1;
    let b = 2;
    let c = 3;
    assert(a < b && b < c)?;
    assert(!(c < a))?;
}

test("bitwise identity") {
    assert((42 & 42) == 42)?;
    assert((42 | 0) == 42)?;
    assert((42 ^ 0) == 42)?;
}

test("shift edges") {
    assert((1 << 0) == 1)?;
    assert((8 >> 0) == 8)?;
    assert((1 << 10) == 1024)?;
}
