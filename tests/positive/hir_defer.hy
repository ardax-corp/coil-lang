// `defer` bodies run as thunks at each later `return`, last first, with
// their captures read from the frame at that moment.
class Log {
    pub n: int,
}

fn push(Log log, int d) {
    log.n = log.n * 10 + d;
}

fn lifo(Log log) {
    defer use (log) {
        push(log, 1);
    }
    defer use (log) {
        push(log, 2);
    }
    push(log, 3);
    return;
}

fn early(Log log, int k) -> int {
    defer use (log) {
        push(log, 1);
    }
    if k == 0 {
        return 0;
    }
    defer use (log, k) {
        push(log, k);
    }
    push(log, 9);
    return k * 2;
}

fn late_value(Log log) -> int {
    let k = 1;
    defer use (log, k) {
        push(log, k);
    }
    k = 7;
    return k;
}

fn falls_through(Log log) {
    defer use (log) {
        let d = 4;
        push(log, d);
    }
    push(log, 5);
}

test("defers run last first") {
    let log = new Log(0);
    lifo(log);
    assert(log.n == 321)?;
}

test("an early return runs only the defers before it") {
    let log = new Log(0);
    assert(early(log, 0) == 0)?;
    assert(log.n == 1)?;
    let log2 = new Log(0);
    assert(early(log2, 6) == 12)?;
    assert(log2.n == 961)?;
}

test("a capture reads its value at the return") {
    let log = new Log(0);
    assert(late_value(log) == 7)?;
    assert(log.n == 7)?;
}

test("defers run on fall-through") {
    let log = new Log(0);
    falls_through(log);
    assert(log.n == 54)?;
}
