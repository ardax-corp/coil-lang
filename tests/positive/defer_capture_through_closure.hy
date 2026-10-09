// A `defer use (p)` of a class parameter, with its function reached
// through a closure or a spawned task (#783).
use task::{scope, Scope};

class Log {
    pub n: int,
}

fn fails(Log log) -> int {
    defer use (log) {
        log.n = log.n + 1;
    }
    return 0;
}

fn call(unit -> int body) -> int {
    return body();
}

test("through a closure") {
    let log = new Log(0);
    let _ = call(fn () use (log) => fails(log));
    assert(log.n == 1)?;
}

test("through a spawned task") {
    let log = new Log(0);
    let _ = scope(
        fn (Scope s) use (log) {
            s.spawn(fn () use (log) => fails(log));
            0
        },
    );
    assert(log.n == 1)?;
}
