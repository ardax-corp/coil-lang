// Values sent with `resume h with v` arrive at the waiting `yield`: a binding
// `yield` can run in a loop, sends may be strings, and the completion value
// can depend on everything sent.
use tests::assert;

gen fn echo() {
    let got = yield 0;
    while got != -1 {
        got = yield got * 2;
    }
    return 100;
}

gen fn accumulate() {
    let total = 0;
    let n = yield total;
    while n > 0 {
        total += n;
        n = yield total;
    }
    return total;
}

gen fn names() {
    let who = yield "ready";
    yield "hello " + who;
    return "bye " + who;
}

test("a binding yield in a loop doubles each sent value") {
    let h = echo();
    assert!((resume h) == 0);
    assert!((resume h with 3) == 6);
    assert!((resume h with 21) == 42);
    assert!(done(h) == false);
    assert!((resume h with -1) == 100);
    assert!(done(h));
}

test("the completion value sums every value sent") {
    let h = accumulate();
    let _ = resume h;
    let _ = resume h with 5;
    let _ = resume h with 7;
    let after = resume h with 30;
    assert!(after == 42);
    let last = resume h with 0;
    assert!(last == 42 && done(h));
}

test("string yields and sends") {
    let h = names();
    assert!((resume h) == "ready");
    assert!((resume h with "ada") == "hello ada");
    assert!((resume h) == "bye ada");
    assert!(done(h));
}

test("two handles keep separate sent state") {
    let a = accumulate();
    let b = accumulate();
    let _ = resume a;
    let _ = resume b;
    let _ = resume a with 1;
    let _ = resume b with 100;
    assert!((resume a with 2) == 3);
    assert!((resume b with 200) == 300);
}
