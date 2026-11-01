// Coroutine bodies in direct MIR: each `yield` is one instruction the VM
// resumes after, with every live word (ints, floats, heap objects) kept in
// the frame across it, collections between resumes included.
use tests::assert;
use gc::collect;

class Box {
    pub v: int,
}

gen fn mixed(int n) {
    let a = n;
    let f = 1.5;
    let b = new Box(n * 10);
    let xs = [n, n + 1, n + 2];
    let i = 0;
    while i < 3 {
        yield a + xs[i] + b.v;
        collect();
        let junk = [i, i, i];
        a = a + junk[0];
        f = f * 2.0;
        i = i + 1;
    }
    if f == 12.0 {
        yield b.v + 1;
    } else {
        yield -1;
    }
    return a;
}

gen fn running_total() {
    let total = 0;
    let more = true;
    while more {
        let got = yield total;
        if got < 0 {
            more = false;
        } else {
            total = total + got;
        }
    }
    return total;
}

gen fn inner(int base) {
    yield base;
    yield base + 1;
}

gen fn chained() {
    let keep = new Box(7);
    yield keep.v;
    collect();
    yield keep.v * 2;
    yield from inner(10);
}

fn half(int n) -> Result<int, string> {
    if n % 2 != 0 {
        return Result::Err("odd");
    }
    return Result::Ok(n / 2);
}

gen fn halves(int n) -> Result<int, string> {
    let h = half(n)?;
    yield Result::Ok(h);
    let q = half(h)?;
    yield Result::Ok(q);
    return Result::Ok(0);
}

gen fn flags() -> int {
    let on = true;
    let off = false;
    yield 1;
    if on == off {
        yield 0;
    }
    if on != off {
        yield 2;
    }
    return 3;
}

fn ok_or(Result<int, string> r, int d) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => d,
    };
}

test("live ints, floats and objects survive yields and collections") {
    let h = mixed(1);
    let a = resume h;
    let b = resume h;
    let c = resume h;
    let d = resume h;
    let e = resume h;
    assert!(a == 12);
    assert!(b == 13);
    assert!(c == 15);
    assert!(d == 11);
    assert!(e == 4);
    assert!(done(h));
}

test("receiving yields in a loop") {
    let h = running_total();
    assert!((resume h) == 0);
    assert!((resume h with 5) == 5);
    assert!((resume h with 7) == 12);
    assert!((resume h with -1) == 12);
    assert!(done(h));
}

test("yield from keeps the delegator's frame") {
    let h = chained();
    let got = 0;
    let i = 0;
    while i < 4 {
        got = got * 100 + (resume h);
        i = i + 1;
    }
    assert!(got == 7141011);
}

test("two-word yields and a gen fn ?") {
    let h = halves(8);
    assert!(ok_or(resume h, -1) == 4);
    assert!(ok_or(resume h, -1) == 2);
    assert!(ok_or(resume h, -1) == 0);
    let odd = halves(6);
    assert!(ok_or(resume odd, -1) == 3);
    assert!(ok_or(resume odd, -1) == -1);
}

test("bool compares in a gen fn") {
    let h = flags();
    assert!((resume h) == 1);
    assert!((resume h) == 2);
    assert!((resume h) == 3);
}
