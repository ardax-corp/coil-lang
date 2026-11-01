// #1137: a `defer` inside a lambda body captures one of the lambda's own
// captures.
use tests::assert;

class Log {
    pub n: int,
}

fn in_lambda(Log log) -> int {
    let f = fn (int y) use (log) {
        defer use (log) {
            log.n = log.n + 1;
        }
        y + 1
    };
    return f(1);
}

test("defer in a lambda") {
    let log = new Log(0);
    assert!(in_lambda(log) == 2);
    assert!(log.n == 1);
}

test("defer in a lambda runs on each call") {
    let log = new Log(10);
    let f = fn (int y) use (log) {
        defer use (log) {
            log.n = log.n * 2;
        }
        y * 3
    };
    assert!(f(2) == 6);
    assert!(f(3) == 9);
    assert!(log.n == 40);
}
