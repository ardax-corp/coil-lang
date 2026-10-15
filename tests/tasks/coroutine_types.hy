// A `gen fn` call is a `coroutine<Y>` of its yield type: `resume` gives a
// yielded value, `for … in` binds each yield, and `resume h with v` sends a
// value of the `yield` expression's type.
use tests::assert;

gen fn coro() {
    yield 1;
}

gen fn ping() {
    let msg = yield "ready";
}

gen fn counter() {
    yield 0;
    yield 1;
}

test("a gen fn call has a coroutine type") {
    let h = coro();
    assert!((typeof h) == "coroutine<int>");
}

test("resume gives the yield type") {
    let h = coro();
    let x = resume h;
    assert!((typeof x) == "int");
    assert!(x == 1);
}

test("resume with sends a value") {
    let h = ping();
    let first = resume h;
    assert!(first == "ready");
    resume h with "hello";
}

test("for in binds the loop variable to the yield type") {
    let sum = 0;
    for x in counter() {
        let y = x;
        assert!((typeof y) == "int");
        sum += y + 1;
    }
    assert!(sum == 3);
}

gen fn yields_then_returns() {
    yield 1;
    return 42;
}

gen fn only_returns() {
    return 42;
}

gen fn annotated() -> int {
    yield 1;
    return 2;
}

test("return in a gen fn has the yield type") {
    let h = yields_then_returns();
    assert!((resume h) == 1);
    assert!((resume h) == 42);
}

test("a return-only gen fn takes its type from the return") {
    let h = only_returns();
    let x = resume h;
    assert!((typeof x) == "int");
    assert!(x == 42);
}

test("done is a bool") {
    let h = coro();
    let d = done(h);
    assert!((typeof d) == "bool");
    assert!(!d);
}

test("a return annotation is the yield type") {
    let h = annotated();
    assert!((typeof h) == "coroutine<int>");
    assert!((resume h) == 1);
}
