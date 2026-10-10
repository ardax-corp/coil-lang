// Early exits that lowering lays out after the function body: the jump to
// the exit inverts and the code after it falls through.
fn fib(int n) -> int {
    if n <= 2 {
        return 1;
    }
    return fib(n - 1) + fib(n - 2);
}

fn first_over([int] xs, int lim) -> int {
    let i = 0;
    while i < len(xs) {
        if xs[i] > lim {
            return i;
        }
        i += 1;
    }
    return -1;
}

static let hits = 0;

// Falls off its end: the moved exit sits behind a jump.
fn count_small(int n) {
    if n > 10 {
        return;
    }
    hits += 1;
}

fn run(() -> int f) -> Option<string> {
    if f() > 3 {
        return Option::Some("big");
    }
    return Option::None;
}

// A closure ahead of a `match` arm that returns, in a loop whose exit
// label ends the body.
fn check(int lim) -> Result<int, string> {
    let i = 0;
    while i < lim {
        i += 1;
        let r = run(fn () use (i) => i);
        match r {
            Option::Some(m) => {
                return Result::Err("failed: " + m);
            },
            Option::None => {},
        }
    }
    return Result::Ok(i);
}

fn half(int n) -> Result<int, string> {
    if n % 2 != 0 {
        return Result::Err("odd");
    }
    return Result::Ok(n / 2);
}

fn quarter(int n) -> Result<int, string> {
    let h = half(n)?;
    return half(h);
}

test("early exits after the body") {
    assert(fib(20) == 6765)?;
    assert(first_over([1, 5, 9, 2], 4) == 1)?;
    assert(first_over([1, 2], 4) == -1)?;
    count_small(3);
    count_small(30);
    count_small(4);
    assert(hits == 2)?;
    assert(check(3) == Result::Ok(3))?;
    assert(check(9) == Result::Err("failed: big"))?;
    assert(quarter(12) == Result::Ok(3))?;
    assert(quarter(6) == Result::Err("odd"))?;
    assert(quarter(5) == Result::Err("odd"))?;
}
