// A `defer use (x)` captures `x` when the `defer` statement runs, as a
// lambda's `use (x)` does: a later assignment is not seen. `use (mut x)`
// borrows `x` instead and sees its value when the defer runs.
use tests::assert;
use task::{scope, Scope, TaskError};

static let LOG: string = "";

fn say(string s) {
    LOG = LOG + s;
}

fn reassigned() -> int {
    let x = 1;
    defer use (x) {
        say("x=" + x.show());
    }
    x = 2;
    return x;
}

test("a reassigned capture keeps its value at the defer") {
    LOG = "";
    assert!(reassigned() == 2);
    assert!(LOG == "x=1");
}

fn borrowed() -> int {
    let x = 1;
    defer use (mut x) {
        say("x=" + x.show());
    }
    x = 2;
    return x;
}

test("a mut capture sees the value at exit") {
    LOG = "";
    assert!(borrowed() == 2);
    assert!(LOG == "x=2");
}

fn early(int k) -> int {
    let n = k;
    defer use (n) {
        say("n=" + n.show());
    }
    n = n * 10;
    if n > 20 {
        return n;
    }
    n = 0;
    return n;
}

test("every exit sees the declared value") {
    LOG = "";
    assert!(early(3) == 30);
    assert!(early(1) == 0);
    assert!(LOG == "n=3n=1");
}

fn param(int p) -> int {
    defer use (p) {
        say("p=" + p.show());
    }
    p = p + 5;
    return p;
}

test("a parameter is captured at the defer") {
    LOG = "";
    assert!(param(1) == 6);
    assert!(LOG == "p=1");
}

fn unwinds() -> int {
    let s = "before";
    defer use (s) {
        say(s);
    }
    s = "after";
    if s == "after" {
        panic "boom";
    }
    return 0;
}

fn failed(Result<int, TaskError> r) -> bool {
    return match r {
        Result::Err(TaskError::Panicked(_)) => true,
        default => false,
    };
}

test("a panic runs the defer with the declared value") {
    LOG = "";
    assert!(failed(scope(fn (Scope s) {
        let _ = s.spawn(fn () => unwinds());
        0
    })));
    assert!(LOG == "before");
}
