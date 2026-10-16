// #1197: `coroutine<Y>` and `coroutine<Y, S>` can be written as types: as a
// `gen fn` return, a parameter, a `Vec` element and a `let` annotation.
use tests::assert;

gen fn counter() -> coroutine<int> {
    yield 0;
    yield 1;
}

gen fn ping() -> coroutine<string, string> {
    let a = yield "ready";
    yield "pong " + a;
}

gen fn plain() -> int {
    yield 9;
}

gen fn quiet() -> coroutine<int, int> {
    yield 4;
}

fn first(coroutine<int> h) -> int {
    return resume h;
}

fn reply(coroutine<string, string> h, string msg) -> string {
    let _ = resume h;
    return resume h with msg;
}

fn make() -> coroutine<int> {
    return counter();
}

test("a coroutine handle is a parameter") {
    let h = counter();
    assert!(first(h) == 0);
    assert!(first(h) == 1);
}

test("a coroutine with a send type is a parameter") {
    assert!(reply(ping(), "ada") == "pong ada");
}

test("a coroutine handle is a return type and a let annotation") {
    let h: coroutine<int> = make();
    let v = resume h;
    assert!(v == 0);
}

test("a coroutine handle is a Vec element") {
    let hs: Vec<coroutine<int>> = [counter(), counter()];
    assert!(first(hs[0]) == 0);
    assert!(first(hs[0]) == 1);
    assert!(first(hs[1]) == 0);
}

test("a bare yield type still works, and a declared send type need not be used") {
    assert!(first(plain()) == 9);
    let q = quiet();
    let v = resume q with 1;
    assert!(v == 4);
}
