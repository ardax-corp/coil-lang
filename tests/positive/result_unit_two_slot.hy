// `Result<(), E>` returns cross direct calls as a `[(), tag]` pair instead
// of a boxed enum. These cover the edges where the pair meets other layouts.
use io::result_unit_probe;

fn check(int x) -> Result<(), string> {
    if x < 0 {
        return Result::Err("neg");
    }
    return;
}

fn check_int(int x) -> Result<(), int> {
    if x > 9 {
        raise x;
    }
    return Result::Ok(());
}

fn chain(int a, int b) -> Result<(), string> {
    check(a)?;
    check(b)?;
    return;
}

fn via_host(int x) -> Result<(), int> {
    return match result_unit_probe(x) {
        Result::Ok(_) => (),
        Result::Err(_) => raise 7,
    };
}

fn pass_through(int x) -> Result<(), string> {
    return check(x)?;
}

fn store_then_return(int x) -> Result<(), string> {
    let r = check(x);
    return match r {
        Result::Ok(_) => (),
        Result::Err(e) => raise e,
    };
}

class Counter {
    pub n: int,
}

impl Counter {
    pub fn bump(int limit) -> Result<(), int> {
        if self.n >= limit {
            raise self.n;
        }
        self.n = self.n + 1;
        return;
    }
}

fn bump_many(Counter c, int k, int limit) -> Result<(), int> {
    let i = 0;
    while i < k {
        c.bump(limit)?;
        i = i + 1;
    }
    return;
}

fn is_ok(Result<(), string> r) -> bool {
    return match r {
        Result::Ok(_) => true,
        Result::Err(_) => false,
    };
}

fn err_of(Result<(), string> r) -> string {
    return match r {
        Result::Ok(_) => "",
        Result::Err(e) => e,
    };
}

test("ok and err round trip") {
    assert(is_ok(check(1)))?;
    assert(err_of(check(-1)) == "neg")?;
    assert(is_ok(chain(1, 2)))?;
    assert(err_of(chain(1, -2)) == "neg")?;
    assert(err_of(pass_through(-3)) == "neg")?;
    assert(err_of(store_then_return(-3)) == "neg")?;
    assert(is_ok(store_then_return(3)))?;
}

test("int error payload") {
    let got = match check_int(12) {
        Result::Ok(_) => -1,
        Result::Err(e) => e,
    };
    assert(got == 12)?;
    let ok = match check_int(3) {
        Result::Ok(_) => 1,
        Result::Err(_) => 0,
    };
    assert(ok == 1)?;
}

test("host result passes through a user function") {
    let a = match via_host(0) {
        Result::Ok(_) => 0,
        Result::Err(e) => e,
    };
    let b = match via_host(-1) {
        Result::Ok(_) => 0,
        Result::Err(e) => e,
    };
    assert(a == 0)?;
    assert(b == 7)?;
}

test("method results chain with ?") {
    let c = new Counter(0);
    assert(
        match bump_many(c, 5, 100) {
            Result::Ok(_) => true,
            Result::Err(_) => false,
        },
    )?;
    assert(c.n == 5)?;
    let stopped = match bump_many(c, 10, 8) {
        Result::Ok(_) => -1,
        Result::Err(n) => n,
    };
    assert(stopped == 8)?;
}

test("results stored in collections and closures") {
    let rs = [check(1), check(-1), check(2)];
    let oks = 0;
    for r in rs {
        if is_ok(r) {
            oks = oks + 1;
        }
    }
    assert(oks == 2)?;
    let f = fn (int x) => check(x);
    assert(err_of(f(-5)) == "neg")?;
    assert(is_ok(f(5)))?;
}

test("? in a test body") {
    check(4)?;
}
