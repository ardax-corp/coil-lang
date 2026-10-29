// A bare `return;` in a `gen fn` finishes the coroutine whatever it
// yields: `done(h)` turns true and the finishing `resume` evaluates to
// the default of the yield type, as falling off the end does (#2088).
use tests::assert;

gen fn take_while(Vec<int> xs, fn(int) -> bool p) {
    for x in xs {
        if !p(x) {
            return;
        }
        yield x;
    }
}

gen fn stop_before(int n) {
    if n < 0 {
        return;
    }
    yield n;
}

gen fn words(bool early) {
    yield "a";
    if early {
        return;
    }
    yield "b";
}

gen fn runs_off() {
    yield 1;
}

test("return; stops a generator consumed by for") {
    let seen = Vec::new();
    for x in take_while(Vec::from([1, 2, 10, 3]), fn (int x) => x < 5) {
        seen.push(x);
    }
    assert!(seen == Vec::from([1, 2]));
}

test("return; before any yield finishes on the first resume") {
    let h = stop_before(-1);
    assert!(done(h) == false);
    let v = resume h;
    assert!(v == 0);
    assert!(done(h) == true);
    let g = stop_before(7);
    assert!((resume g) == 7);
    assert!(done(g) == false);
}

test("return; after a yield agrees with falling off the end") {
    let h = take_while(Vec::from([1, 10]), fn (int x) => x < 5);
    assert!((resume h) == 1);
    let fin = resume h;
    assert!(fin == 0);
    assert!(done(h) == true);
    let g = runs_off();
    assert!((resume g) == 1);
    let off = resume g;
    assert!(off == 0);
    assert!(done(g) == true);
}

test("return; in a generator yielding strings") {
    let seen = Vec::new();
    for w in words(true) {
        seen.push(w);
    }
    assert!(seen == Vec::from(["a"]));
    let all = Vec::new();
    for w in words(false) {
        all.push(w);
    }
    assert!(all == Vec::from(["a", "b"]));
}
