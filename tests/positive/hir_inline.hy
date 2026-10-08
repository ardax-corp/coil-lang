// Typed inlining (`COIL_HIR_INLINE=1`): small callees spliced into their
// callers must keep argument order, receiver mutation and side effects.
class Counter {
    pub n: int,
}

impl Counter {
    pub fn get() -> int {
        return self.n;
    }

    pub fn bump(int by) {
        self.n = self.n + by;
    }

    pub fn next() -> int {
        self.n = self.n + 1;
        return self.n;
    }
}

static let trace: int = 0;

fn note(int d) -> int {
    trace = trace * 10 + d;
    return d;
}

fn pick(bool c, int a, int b) -> int {
    let r = b;
    if c {
        r = a;
    }
    return r * 2;
}

fn shadow(int x) -> int {
    let x2 = x + 1;
    x = x2 * 3;
    return x;
}

fn sum_loop(int n) -> int {
    let c = new Counter(0);
    let acc = 0;
    let i = 0;
    while i < n {
        c.bump(2);
        acc = acc + c.get() + pick(i % 2 == 0, i, -i);
        i = i + 1;
    }
    return acc;
}

fn order() -> int {
    trace = 0;
    return note(1) + note(2) * note(3);
}

test("receiver methods and branches inline") {
    assert(sum_loop(4) == (2 + 0) + (4 - 2) + (6 + 4) + (8 - 6))?;
}

test("calls keep their order") {
    assert(order() == 7)?;
    assert(trace == 123)?;
}

test("a rebound parameter does not write the argument") {
    let x = 4;
    assert(shadow(x) == 15)?;
    assert(x == 4)?;
}

test("a receiver mutated by the callee is the caller's object") {
    let c = new Counter(5);
    let a = c.next();
    let b = c.next() + c.get();
    assert(a == 6)?;
    assert(b == 14)?;
    assert(c.n == 7)?;
}
