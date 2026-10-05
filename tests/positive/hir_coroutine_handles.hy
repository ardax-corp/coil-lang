// A coroutine handle is one word: `MakeCoro` for the call, then
// `ResumeCoro` (with or without a sent value) and `DoneCoro`.
async fn upto(int n) {
    let i = 0;
    while i < n {
        yield i;
        i = i + 1;
    }
    return -1;
}

async fn echo() {
    let x = yield 0;
    while true {
        x = yield x * 2;
    }
}

fn sum_upto(int n) -> int {
    let h = upto(n);
    let acc = 0;
    let v = resume h;
    while !done(h) {
        if v >= 0 {
            acc = acc + v;
        }
        v = resume h;
    }
    return acc;
}

fn doubled(int a, int b) -> int {
    let h = echo();
    resume h;
    let x = resume h with a;
    let y = resume h with b;
    return x + y;
}

fn fresh_not_done() -> bool {
    let h = upto(3);
    return done(h);
}

test("resume until done") {
    assert(sum_upto(5) == 10)?;
    assert(sum_upto(0) == 0)?;
}

test("resume with a sent value") {
    assert(doubled(3, 4) == 14)?;
}

test("a fresh handle is not done") {
    assert(!fresh_not_done())?;
}
