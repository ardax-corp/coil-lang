// A `()`-typed value returned from a niche unit `Result` function
// (`return check(x)?`, a unit `match`) lowers from HIR: the value runs for
// its effect, then the function returns `Ok(())`.
class Counter {
    pub n: int,
}

fn check(int x) -> Result<(), string> {
    if x < 0 {
        return Result::Err("negative");
    }
    return Result::Ok(());
}

fn pass_through(int x) -> Result<(), string> {
    return check(x)?;
}

fn via_match(int x) -> Result<(), string> {
    let r = check(x);
    return match r {
        Result::Ok(_) => (),
        Result::Err(e) => raise e,
    };
}

fn bump(Counter c) {
    c.n = c.n + 1;
}

fn bump_twice(Counter c) {
    bump(c);
    return bump(c);
}

fn err_of(Result<(), string> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
}

test("unit value returned as Ok") {
    assert(err_of(pass_through(1)) == "ok")?;
    assert(err_of(pass_through(-1)) == "negative")?;
    assert(err_of(via_match(2)) == "ok")?;
    assert(err_of(via_match(-2)) == "negative")?;
}

test("unit call returned from a unit function") {
    let c = new Counter(0);
    bump_twice(c);
    assert(c.n == 2)?;
}
