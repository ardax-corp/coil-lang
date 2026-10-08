// T2: a panic runs the `defer`s of the frames it leaves, innermost first.
// A child task's panic unwinds only that task, so a test can watch the log.
// The log is a static `int`: a class capture through a function value is
// coil-lang#783.
use task::{scope, Scope, TaskError};

static let LOG: int = 0;

fn push(int d) {
    LOG = LOG * 10 + d;
}

fn fails(int k) -> int {
    defer {
        push(1);
    }
    if k > 0 {
        panic "fails";
    }
    return 0;
}

fn calls_fails() -> int {
    defer {
        push(2);
    }
    let r = fails(1);
    push(9);
    return r;
}

fn skipped(bool arm) -> int {
    defer {
        push(3);
    }
    if arm {
        defer {
            push(4);
        }
    }
    panic "skipped";
}

class Counter {
    pub step: int,
}

impl Counter {
    pub fn bump() -> int {
        // No `use (self)`: a class capture through a function value is #783.
        defer {
            push(5);
        }
        panic "method";
    }
}

fn generic<T>(T value) -> T {
    defer {
        push(6);
    }
    panic "generic";
}

gen fn producer() -> int {
    defer {
        push(7);
    }
    yield 1;
    panic "generator";
}

fn drain() -> int {
    let g = producer();
    resume g;
    resume g;
    return 0;
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

test("a panic runs the defers of each frame it leaves") {
    LOG = 0;
    assert(run(calls_fails))?;
    assert(LOG == 12)?;
}

test("a defer whose statement did not run stays out") {
    LOG = 0;
    assert(run(fn () => skipped(false)))?;
    assert(LOG == 3)?;
    LOG = 0;
    assert(run(fn () => skipped(true)))?;
    assert(LOG == 43)?;
}

test("methods and generic instances unwind") {
    LOG = 0;
    let c = new Counter(5);
    assert(run(fn () use (c) => c.bump()))?;
    assert(run(fn () => generic(4)))?;
    assert(LOG == 56)?;
}

test("a generator's defers run when it panics") {
    LOG = 0;
    assert(run(drain))?;
    assert(LOG == 7)?;
}

test("without a panic, defers still run once at the return") {
    LOG = 0;
    assert(fails(0) == 0)?;
    assert(LOG == 1)?;
}
