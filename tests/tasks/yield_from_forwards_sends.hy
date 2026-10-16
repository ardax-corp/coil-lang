// #1196: `yield from` a delegate that receives sends takes the delegate's
// send type, and `resume … with v` reaches the delegate through the chain.
use tests::assert;

gen fn inner() {
    let a = yield 1;
    yield a + 10;
}

gen fn outer() {
    yield from inner();
}

gen fn top() {
    yield from outer();
}

gen fn echo_twice() {
    let a = yield 0;
    let b = yield a;
    yield a + b;
}

gen fn wrapped() {
    yield 100;
    yield from echo_twice();
    yield 200;
}

test("a send reaches the delegate") {
    let h = outer();
    let x = resume h;
    let y = resume h with 5;
    assert!(x == 1);
    assert!(y == 15);
}

test("a send passes through two levels of delegation") {
    let h = top();
    let x = resume h;
    let y = resume h with 7;
    assert!(x == 1);
    assert!(y == 17);
}

test("sends reach a delegate between the delegator's own yields") {
    let h = wrapped();
    let a = resume h;
    let b = resume h with 0;
    let c = resume h with 3;
    let d = resume h with 4;
    let e = resume h with 0;
    assert!(a == 100);
    assert!(b == 0);
    assert!(c == 3);
    assert!(d == 7);
    assert!(e == 200);
}
