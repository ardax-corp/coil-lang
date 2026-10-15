// #1198: a three-level `yield from` chain (a delegate that itself
// delegates) passes every value through and completes.
use tests::assert;

gen fn leaf() {
    yield 1;
    yield 2;
}

gen fn mid() {
    yield from leaf();
}

gen fn top() {
    yield from mid();
}

gen fn three(int base) {
    yield base;
    yield base + 1;
}

gen fn layer(int base) {
    yield from three(base);
    yield base + 5;
}

gen fn deep() {
    yield from layer(10);
    yield from layer(20);
}

test("values pass through a three-level chain and it completes") {
    let h = top();
    let a = resume h;
    let b = resume h;
    assert!(a == 1);
    assert!(b == 2);
    assert!(!done(h));
    let _ = resume h;
    assert!(done(h));
}

test("each level continues after its delegate finishes") {
    let got = [0, 0, 0, 0, 0, 0];
    let n = 0;
    for v in deep() {
        got[n] = v;
        n = n + 1;
    }
    assert!(n == 6);
    assert!(got[0] == 10);
    assert!(got[1] == 11);
    assert!(got[2] == 15);
    assert!(got[3] == 20);
    assert!(got[4] == 21);
    assert!(got[5] == 25);
}
