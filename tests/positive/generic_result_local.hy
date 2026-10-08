// A generic fn whose body annotates a local with its type parameter
// (`Result<T, E>` holding an `Err`, `Option<T>` holding `None`) used to
// drop `T` from its scheme: every call shared one `T`, so a call at `int`
// read a two-slot `Result` the body never returned (#789).
fn gerr<T>(T x) -> Result<T, string> {
    let e: Result<T, string> = Result::Err("failed");
    return e;
}

fn gpick<T>(T x, bool fail) -> Result<T, string> {
    if fail {
        let e: Result<T, string> = Result::Err("failed");
        return e;
    }
    let r: Result<T, string> = Result::Ok(x);
    return r;
}

fn greassign<T>(T x, bool fail) -> Result<T, string> {
    let r: Result<T, string> = Result::Ok(x);
    if fail {
        r = Result::Err("failed");
    }
    return r;
}

fn gnone<T>(T x) -> Option<T> {
    let n: Option<T> = Option::None;
    return n;
}

fn show(Result<int, string> r) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
}

fn show_str(Result<string, string> r) -> string {
    return match r {
        Result::Ok(v) => v,
        Result::Err(e) => e,
    };
}

test("an Err local returned at int and at string") {
    assert(show(gerr(3)) == -1)?;
    assert(show_str(gerr("x")) == "failed")?;
}

test("Ok and Err locals on two paths") {
    assert(show(gpick(3, false)) == 3)?;
    assert(show(gpick(3, true)) == -1)?;
    assert(show_str(gpick("y", false)) == "y")?;
}

test("a Result local reassigned in an if") {
    assert(show(greassign(4, false)) == 4)?;
    assert(show(greassign(4, true)) == -1)?;
}

test("a None local returned at two types") {
    let a = gnone(1);
    let b = gnone("s");
    let both_none = match a {
        Option::None => match b {
            Option::None => true,
            Option::Some(_) => false,
        },
        Option::Some(_) => false,
    };
    assert(both_none)?;
}
