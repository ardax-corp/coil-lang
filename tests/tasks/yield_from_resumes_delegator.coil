// #961: after a `yield from` delegate finishes, the delegating coroutine
// continues after the `yield from` (with its locals intact), and the
// resumer keeps running.
use tests::assert;

class Box {
    pub v: int,
}

gen fn inner(int base) {
    yield base;
    yield base + 1;
}

gen fn chained() {
    let keep = new Box(7);
    yield from inner(10);
    yield keep.v;
    yield from inner(20);
    yield keep.v * 2;
}

gen fn finishes_at_once() -> int {
    return 5;
}

gen fn delegate_to_empty() {
    yield 1;
    yield from finishes_at_once();
    yield 2;
}

gen fn middle() {
    yield 100;
    yield from inner(200);
    yield 300;
}

gen fn nested() {
    let local = 9;
    yield from middle();
    yield local;
}

test("delegator continues after the delegate finishes") {
    let h = chained();
    let got = [0, 0, 0, 0, 0, 0];
    let i = 0;
    while i < 6 {
        got[i] = resume h;
        i = i + 1;
    }
    assert!(got[0] == 10);
    assert!(got[1] == 11);
    assert!(got[2] == 7);
    assert!(got[3] == 20);
    assert!(got[4] == 21);
    assert!(got[5] == 14);
    assert!(!done(h));
    let _ = resume h;
    assert!(done(h));
}

test("the resumer keeps running after a delegate finishes") {
    let h = chained();
    let sum = 0;
    let steps = 0;
    while !done(h) {
        let v = resume h;
        sum = sum + v;
        steps = steps + 1;
    }
    assert!(steps == 7);
    assert!(sum == 10 + 11 + 7 + 20 + 21 + 14);
}

test("a delegate that finishes without yielding") {
    let h = delegate_to_empty();
    let r1 = resume h;
    assert!(r1 == 1);
    let r2 = resume h;
    assert!(r2 == 2);
}

test("nested delegation resumes each level") {
    let h = nested();
    let r3 = resume h;
    assert!(r3 == 100);
    let r4 = resume h;
    assert!(r4 == 200);
    let r5 = resume h;
    assert!(r5 == 201);
    let r6 = resume h;
    assert!(r6 == 300);
    let r7 = resume h;
    assert!(r7 == 9);
}
