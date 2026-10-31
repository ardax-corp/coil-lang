// A panic runs a body's armed `defer`s with their captures as they stand
// at the panic: a capture assigned after the `defer`, one armed in a loop,
// and a heap capture held only for the `defer` across a collection.
use tests::assert;
use task::{scope, Scope, TaskError};
use string::format;

static let LOG: int = 0;

fn push(int d) {
    LOG = LOG * 10 + d;
}

class Cell {
    pub n: int,
}

fn late(int k) -> int {
    let x = 1;
    defer use (x) {
        push(x);
    }
    x = 2;
    if k > 0 {
        x = 3;
        panic "late";
    }
    return x;
}

fn looped(int stop) -> int {
    let i = 0;
    while i < 5 {
        defer use (i) {
            push(i + 1);
        }
        if i == stop {
            panic "looped";
        }
        i = i + 1;
    }
    return i;
}

fn churn(int n) -> int {
    let s = 0;
    let i = 0;
    while i < n {
        let c = new Cell(i);
        s = s + c.n;
        i = i + 1;
    }
    return s;
}

fn held(int k) -> int {
    let s = format("%v", k % 10 + 7);
    defer use (s) {
        if s == "7" {
            push(7);
        }
    }
    let t = churn(k);
    if t >= 0 {
        panic "held";
    }
    return t;
}

fn ordered(bool arm, int k) -> int {
    let a = 4;
    defer use (a) {
        push(a);
    }
    if arm {
        let b = 5;
        defer use (b) {
            push(b);
        }
    }
    a = k;
    panic "ordered";
}

fn failed(Result<int, TaskError> r) -> bool {
    return match r {
        Result::Ok(_) => false,
        Result::Err(_) => true,
    };
}

// Run `body` as a child task: its panic fails the scope instead of the VM.
fn run(unit -> int body) -> bool {
    return failed(scope(fn (Scope s) use (body) {
        let _ = s.spawn(body);
        0
    }));
}

test("a capture assigned after its defer is read at the panic") {
    LOG = 0;
    assert!(run(fn () => late(1)));
    assert!(LOG == 3);
    LOG = 0;
    assert!(late(0) == 2);
    assert!(LOG == 2);
}

test("a defer armed in a loop runs once, with the loop's value") {
    LOG = 0;
    assert!(run(fn () => looped(2)));
    assert!(LOG == 3);
}

test("a heap capture survives a collection before the panic") {
    LOG = 0;
    assert!(run(fn () => held(20000)));
    assert!(LOG == 7);
}

test("armed defers run last first; an unarmed one stays out") {
    LOG = 0;
    assert!(run(fn () => ordered(true, 6)));
    assert!(LOG == 56);
    LOG = 0;
    assert!(run(fn () => ordered(false, 8)));
    assert!(LOG == 8);
}
