// gen fn / yield / resume / done / yield from / send.
gen fn counter() {
    yield 1;
    yield 2;
    return 42;
}

gen fn sender() {
    let x = yield 0;
    yield x;
    return x + 1;
}

gen fn gen_three() {
    yield 0;
    yield 1;
    yield 2;
}

gen fn outer() {
    yield from gen_three();
}

gen fn parameterized(int base) {
    yield base;
    yield base + 1;
    yield base + 2;
}

test("resume yields then return then done sentinel") {
    let h = counter();
    let a = resume h;
    assert(a == 1)?;
    let b = resume h;
    assert(b == 2)?;
    let c = resume h;
    assert(c == 42)?;
    assert(done(h) == true)?;
}

test("done builtin") {
    let h = counter();
    assert(done(h) == false)?;
    let _ = resume h;
    let _ = resume h;
    let _ = resume h;
    assert(done(h) == true)?;
}

test("resume with send") {
    let h = sender();
    let a = resume h;
    assert(a == 0)?;
    let b = resume h with 10;
    assert(b == 10)?;
    let c = resume h;
    assert(c == 11)?;
}

test("yield from delegates") {
    let h = outer();
    let a = resume h;
    assert(a == 0)?;
    let b = resume h;
    assert(b == 1)?;
    let c = resume h;
    assert(c == 2)?;
}

test("parameterized interleaved handles") {
    let a = parameterized(10);
    let b = parameterized(100);
    let a0 = resume a;
    assert(a0 == 10)?;
    let b0 = resume b;
    assert(b0 == 100)?;
    let a1 = resume a;
    assert(a1 == 11)?;
    let b1 = resume b;
    assert(b1 == 101)?;
    let a2 = resume a;
    assert(a2 == 12)?;
    let b2 = resume b;
    assert(b2 == 102)?;
}

test("for in coroutine") {
    let sum = 0;
    for v in gen_three() {
        sum = sum + v;
    }
    assert(sum == 3)?;
}

gen fn once() {
    return 99;
}

test("done false before first resume") {
    let h = counter();
    assert(done(h) == false)?;
}

test("immediate return gen completes on first resume") {
    let h = once();
    assert(done(h) == false)?;
    let v = resume h;
    assert(v == 99)?;
    assert(done(h) == true)?;
}
