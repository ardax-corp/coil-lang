// A call through a function value whose result is a one-word enum gets
// that word back, as a direct call does.

fn check(int x) -> Result<(), string> {
    if x < 0 {
        return Result::Err("neg");
    }
    return;
}

fn err_of(Result<(), string> r) -> string {
    return match r {
        Result::Ok(_) => "",
        Result::Err(e) => e,
    };
}

fn run(int -> Result<(), string> f, int x) -> string {
    return err_of(f(x));
}

test("closure over a unit result") {
    let f = fn (int x) => check(x);
    assert(err_of(f(-5)) == "neg")?;
    assert(err_of(f(5)) == "")?;
}

test("function value parameter") {
    assert(run(check, -1) == "neg")?;
    assert(run(fn (int x) => check(x + 10), -3) == "")?;
}

test("result of a closure with ?") {
    let f = fn (int x) => check(x);
    f(1)?;
    assert(err_of(f(-2)) == "neg")?;
}
