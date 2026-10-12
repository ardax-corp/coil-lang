// Generic bodies and test bodies return a niche `Option` / `Result` boxed:
// each return builds the enum object, on every path.
fn gfind<T>(Vec<T> xs, int at) -> Option<T> {
    if at < len(xs) {
        let s: Option<T> = Option::Some(xs[at]);
        return s;
    }
    let n: Option<T> = Option::None;
    return n;
}

fn gpass<T>(T x, bool fail) -> Result<(), string> {
    if fail {
        return Result::Err("no");
    }
    let _y = x;
    return;
}

fn gpick<T>(T x, bool fail) -> Result<T, string> {
    if fail {
        let e: Result<T, string> = Result::Err("bad");
        return e;
    }
    let r: Result<T, string> = Result::Ok(x);
    return r;
}

fn name(Option<string> o) -> string {
    return match o {
        Option::Some(s) => s,
        Option::None => "-",
    };
}

fn passed(Result<(), string> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
}

fn picked(Result<string, string> r) -> string {
    return match r {
        Result::Ok(s) => s,
        Result::Err(e) => e,
    };
}

fn check(int x) -> Result<(), string> {
    if x < 0 {
        return Result::Err("neg");
    }
    return;
}

test("a boxed Option on both paths") {
    let xs: Vec<string> = Vec::new();
    xs.push("a");
    assert(name(gfind(xs, 0)) == "a")?;
    assert(name(gfind(xs, 1)) == "-")?;
}

test("a boxed unit Result on both paths") {
    assert(passed(gpass("s", false)) == "ok")?;
    assert(passed(gpass("s", true)) == "no")?;
}

test("a boxed Result on both paths") {
    assert(picked(gpick("y", false)) == "y")?;
    assert(picked(gpick("y", true)) == "bad")?;
}

test("? and a fall-off Ok in a test body") {
    check(4)?;
    check(0)?;
}

test("an early return of a Result in a test body") {
    if passed(check(-1)) == "neg" {
        return check(1);
    }
    assert(false)?;
}
