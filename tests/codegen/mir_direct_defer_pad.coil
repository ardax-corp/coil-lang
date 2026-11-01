// A body HIR lowers with a `defer` gets its cleanup pad from HIR, not
// from the thunk its stack IL registers: defers on a branch, in a loop
// and after an early return run on each exit.
use tests::assert;

class Log {
    pub n: int,
}

fn branch(Log log, bool on) -> int {
    if on {
        defer use (log) {
            log.n = log.n + 1;
        }
        return 1;
    }
    return 0;
}

fn looped(Log log, int n) -> int {
    let total = 0;
    for i in 0..n {
        defer use (log) {
            log.n = log.n + 10;
        }
        total = total + i;
    }
    return total;
}

fn early(Log log, int x) -> int {
    if x < 0 {
        return -1;
    }
    defer use (log) {
        log.n = log.n + 100;
    }
    return x;
}

test("defer on a branch") {
    let log = new Log(0);
    assert!(branch(log, true) == 1);
    assert!(branch(log, false) == 0);
    assert!(log.n == 1);
}

test("defer in a loop") {
    let log = new Log(0);
    assert!(looped(log, 3) == 3);
    assert!(log.n >= 10);
}

test("defer after an early return") {
    let log = new Log(0);
    assert!(early(log, -1) == -1);
    assert!(log.n == 0);
    assert!(early(log, 4) == 4);
    assert!(log.n == 100);
}
