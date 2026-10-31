// A `defer` armed inside nested constant-range `for` loops runs once on
// the `return` that leaves them (coil-lang#1148).
use tests::assert;

class Log {
    pub n: int,
}

fn find(Log log, int stop) -> int {
    for i in 0..5 {
        for j in 0..5 {
            if j == stop {
                defer use (log) {
                    log.n = log.n * 10 + 1;
                }
                return i * 10 + j;
            }
        }
    }
    return -1;
}

fn three_deep(Log log, int stop) -> int {
    for a in 0..3 {
        for b in 0..3 {
            for c in 0..3 {
                if a + b + c == stop {
                    defer use (log) {
                        log.n = log.n * 10 + 2;
                    }
                    return a * 100 + b * 10 + c;
                }
            }
        }
    }
    return -1;
}

test("defer inside two nested range loops runs once") {
    let log = new Log(0);
    assert!(find(log, 2) == 2);
    assert!(log.n == 1);
}

test("defer inside nested range loops is skipped when never armed") {
    let log = new Log(0);
    assert!(find(log, 7) == -1);
    assert!(log.n == 0);
}

test("defer inside three nested range loops") {
    let log = new Log(0);
    assert!(three_deep(log, 4) == 22);
    assert!(log.n == 2);
}
