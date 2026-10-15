// `defer` bodies with direct MIR: each exit runs the armed defers lowered
// before it, last first, captures read at the exit; a defer armed in a
// loop runs once.
use tests::assert;

class Log {
    pub n: int,
}

fn push(Log log, int d) {
    log.n = log.n * 10 + d;
}

fn in_loop(Log log, int stop) -> int {
    let i = 0;
    while i < 5 {
        defer use (log, i) {
            let shown = i + 1;
            push(log, shown);
        }
        if i == stop {
            return i;
        }
        i = i + 1;
    }
    return -1;
}

fn guarded(Log log) -> Option<int> {
    defer use (log) {
        push(log, 2);
    }
    let start = 3;
    defer use (log, start) {
        push(log, start);
    }
    let v = Option::Some(start)?;
    push(log, 1);
    return Option::Some(v);
}

fn missing(Log log, Option<int> o) -> Option<int> {
    defer use (log) {
        push(log, 5);
    }
    let v = o?;
    push(log, v);
    return Option::Some(v * 2);
}

test("a defer armed in a loop runs once, with the capture at the exit") {
    let log = new Log(0);
    assert!(in_loop(log, 2) == 2);
    assert!(log.n == 3);
    let log2 = new Log(0);
    assert!(in_loop(log2, 9) == -1);
    assert!(log2.n == 6);
}

test("defers run last first on a fall-through and a ? exit") {
    let log = new Log(0);
    let got = match guarded(log) {
        Option::Some(v) => v,
        Option::None => -1,
    };
    assert!(got == 3);
    assert!(log.n == 132);
    let log2 = new Log(0);
    let miss = match missing(log2, Option::None) {
        Option::Some(v) => v,
        Option::None => -1,
    };
    assert!(miss == -1);
    assert!(log2.n == 5);
    let log3 = new Log(0);
    let hit = match missing(log3, Option::Some(4)) {
        Option::Some(v) => v,
        Option::None => -1,
    };
    assert!(hit == 8);
    assert!(log3.n == 45);
}
