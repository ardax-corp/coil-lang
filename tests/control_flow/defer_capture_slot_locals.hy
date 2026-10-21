// A `defer` captures a local that would otherwise live in frame slots
// rather than as one word: a class from `new`, a fixed array, an `Option`
// or a tuple, also from a nested block (coil-lang#1758). The capture keeps
// the local one word, and the defer reads it at the `return`.
use tests::assert;

class P {
    pub a: int,
    pub b: int,
}

readonly class Q {
    pub a: int,
}

fn mk(int a) -> P {
    return new P(a, a + 1);
}

fn from_call(Vec<int> log) {
    let p = mk(3);
    {
        defer use (p, log) {
            log.push(p.a);
        }
    }
}

fn from_new(Vec<int> log) {
    let p = new P(4, 5);
    {
        defer use (p, log) {
            log.push(p.b);
        }
    }
}

fn readonly_new(Vec<int> log) -> int {
    let q = new Q(6);
    defer use (q, log) {
        log.push(q.a);
    }
    return q.a + 1;
}

fn late_field(Vec<int> log) {
    let p = new P(1, 2);
    defer use (p, log) {
        log.push(p.a);
    }
    p.a = 8;
}

fn array(Vec<int> log) {
    let xs = [1, 9, 3];
    defer use (xs, log) {
        log.push(xs[1]);
    }
}

fn option(Vec<int> log, int k) {
    let o = Some(k);
    defer use (o, log) {
        match o {
            Some(v) => log.push(v),
            None => log.push(0),
        }
    }
}

fn tuple(Vec<int> log) {
    let t = (4, 11);
    defer use (t, log) {
        log.push(t[1]);
    }
}

test("a defer captures a class local from a call or `new`") {
    let log: Vec<int> = [];
    from_call(log);
    from_new(log);
    assert!(readonly_new(log) == 7);
    late_field(log);
    assert!(log.len() == 4);
    assert!(log[0] == 3);
    assert!(log[1] == 5);
    assert!(log[2] == 6);
    assert!(log[3] == 8);
}

test("a defer captures an array, `Option` or tuple local") {
    let log: Vec<int> = [];
    array(log);
    option(log, 10);
    tuple(log);
    assert!(log.len() == 3);
    assert!(log[0] == 9);
    assert!(log[1] == 10);
    assert!(log[2] == 11);
}

test("a defer in a test body captures a `new` class local") {
    let log: Vec<int> = [];
    let p = new P(12, 13);
    {
        defer use (p, log) {
            log.push(p.a);
        }
    }
    assert!(log.len() == 0);
}
