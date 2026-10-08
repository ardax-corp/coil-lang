// T2: a panic runs the `defer`s of the frames it leaves, innermost first.
// A child task's panic unwinds that task only, so the scope sees the log.
use task::{scope, Scope, TaskError};

class Log {
    pub n: int,
}

fn push(Log log, int d) {
    log.n = log.n * 10 + d;
}

fn fails(Log log, int k) -> int {
    defer use (log) {
        push(log, 1);
    }
    if k > 0 {
        panic "fails";
    }
    return 0;
}

fn calls_fails(Log log) -> int {
    defer use (log) {
        push(log, 2);
    }
    let r = fails(log, 1);
    push(log, 9);
    return r;
}

fn skipped(Log log, bool arm) -> int {
    defer use (log) {
        push(log, 3);
    }
    if arm {
        defer use (log) {
            push(log, 4);
        }
    }
    panic "skipped";
}

class Counter {
    pub log: Log,
}

impl Counter {
    pub fn bump() -> int {
        defer use (self) {
            push(self.log, 5);
        }
        panic "method";
    }
}

fn generic<T>(Log log, T value) -> T {
    defer use (log) {
        push(log, 6);
    }
    panic "generic";
}

gen fn producer(Log log) -> int {
    defer use (log) {
        push(log, 7);
    }
    yield 1;
    panic "generator";
}

fn drain(Log log) -> int {
    let g = producer(log);
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

fn run(unit -> int body) -> bool {
    return failed(scope(fn (Scope s) use (body) {
        let _ = s.spawn(body);
        0
    }));
}

test("a panic runs the defers of each frame it leaves") {
    let log = new Log(0);
    assert(run(fn () use (log) => calls_fails(log)))?;
    assert(log.n == 12)?;
}

test("a defer whose statement did not run stays out") {
    let log = new Log(0);
    assert(run(fn () use (log) => skipped(log, false)))?;
    assert(log.n == 3)?;
    let log2 = new Log(0);
    assert(run(fn () use (log2) => skipped(log2, true)))?;
    assert(log2.n == 43)?;
}

test("methods and generic instances unwind") {
    let log = new Log(0);
    let c = new Counter(log);
    assert(run(fn () use (c) => c.bump()))?;
    assert(run(fn () use (log) => generic(log, 4)))?;
    assert(log.n == 56)?;
}

test("a generator's defers run when it panics") {
    let log = new Log(0);
    assert(run(fn () use (log) => drain(log)))?;
    assert(log.n == 7)?;
}

test("without a panic, defers still run once at the return") {
    let log = new Log(0);
    assert(fails(log, 0) == 0)?;
    assert(log.n == 1)?;
}
