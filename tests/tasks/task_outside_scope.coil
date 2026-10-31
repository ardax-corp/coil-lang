// Outside any `task::scope`, `task::sleep` sleeps the thread and
// `task::yield_now` does nothing; a scope can run from a plain helper
// called in a loop.
use tests::assert;
use task::{scope, Scope, TaskError, sleep, yield_now};
use clock::mono_nanos;

test("sleep outside a scope waits at least the time asked") {
    let t0 = mono_nanos();
    sleep(10);
    let waited = mono_nanos() - t0;
    assert!(waited >= 10 * 1000 * 1000);
}

test("yield_now outside a scope returns at once") {
    let n = 0;
    while n < 100 {
        yield_now();
        n += 1;
    }
    assert!(n == 100);
}

fn run_once(int k) -> int {
    let r = scope(fn (Scope s) use (k) {
        let t = s.spawn(fn () use (k) => k * k);
        t.join()
    });
    return match r {
        Result::Ok(Result::Ok(v)) => v,
        default => -1,
    };
}

test("scopes run one after another") {
    let total = 0;
    for k in 1..6 {
        total += run_once(k);
    }
    assert!(total == 55);
}
