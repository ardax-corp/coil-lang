// Integer / float arithmetic — VM binary ops + peephole fusions.
test("int add sub mul") {
    assert(1 + 2 == 3)?;
    assert(10 - 3 == 7)?;
    assert(6 * 7 == 42)?;
}

test("int div mod") {
    assert(20 / 4 == 5)?;
    assert(20 % 6 == 2)?;
    assert(7 % 7 == 0)?;
}

test("int power and associativity") {
    assert(2 ** 3 == 8)?;
    assert(2 ** 3 ** 2 == 512)?; // right-assoc: 2**(3**2)
    assert((2 ** 3) ** 2 == 64)?;
}

test("unary plus minus") {
    assert(-5 + 8 == 3)?;
    assert(+7 == 7)?;
    assert(-(-3) == 3)?;
}

// Scalar `-` on floats must use NEGF (int NEG two's-complements IEEE bits:
// `-0.55` used to evaluate as `-7.6`). Compare against `0.0 - x` (SUBF).
test("float unary minus") {
    assert(-0.55 == 0.0 - 0.55)?;
    assert(-1.5 == 0.0 - 1.5)?;
    assert(-(-0.55) == 0.55)?;
    let x = 0.55;
    assert(-x == 0.0 - x)?;
    assert(-x + x == 0.0)?;
}

test("chained int arithmetic") {
    assert(1 + 2 * 3 == 7)?;
    assert((1 + 2) * 3 == 9)?;
    assert(100 - 50 - 25 == 25)?; // left-assoc
}

test("float arithmetic") {
    assert(1.5 + 2.5 == 4.0)?;
    assert(5.0 - 1.5 == 3.5)?;
    assert(2.0 * 3.0 == 6.0)?;
    assert(9.0 / 2.0 == 4.5)?;
}

test("float power") {
    assert(2.0 ** 3.0 == 8.0)?;
}

// Literal `**` const-folds; variable operands lower to a fused slot-pair op,
// which is a separate VM path.
test("float power on locals") {
    let base = 2.0;
    let exp = 10.0;
    let p = base ** exp;
    assert(p == 1024.0)?;
    assert(base ** exp == 1024.0)?;
}

fn fpow(float base, float exp) -> float {
    return base ** exp;
}

// Return-site fuse (`BinReturn` + PowF) is a third path that used to yield 0.0.
test("float power via return") {
    assert(fpow(2.0, 8.0) == 256.0)?;
    assert(fpow(3.0, 4.0) == 81.0)?;
}

test("mixed locals preserve slots") {
    let a = 10;
    let b = 20;
    let c = a + b;
    assert(c == 30)?;
    assert(a == 10)?;
    assert(b == 20)?;
}

test("compound assignment arithmetic") {
    let x = 5;
    x += 3;
    assert(x == 8)?;
    x -= 2;
    assert(x == 6)?;
    x *= 3;
    assert(x == 18)?;
    x /= 2;
    assert(x == 9)?;
    x %= 5;
    assert(x == 4)?;
    x **= 2;
    assert(x == 16)?;
}
