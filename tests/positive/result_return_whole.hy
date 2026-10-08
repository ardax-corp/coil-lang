// In a function returning `Result<T, E>`, `return v;` is the Ok payload,
// but a `v` that is already a `Result<T, E>` is returned as is (#786).
class Box<R> {
    pub v: R,
}

impl Box<R> {
    pub fn get() -> Result<R, string> {
        return Result::Ok(self.v);
    }
}

fn get_it<T>(Box<T> b) -> Result<T, string> {
    return b.get();
}

fn check(int n) -> Result<int, string> {
    if n < 0 {
        return Result::Err("negative");
    }
    return Result::Ok(n * 2);
}

fn forward(int n) -> Result<int, string> {
    let r = check(n);
    return r;
}

fn forward_or_payload(int n) -> Result<int, string> {
    if n == 0 {
        return 100;
    }
    return check(n);
}

fn ok_or(Result<int, string> r, int fallback) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => fallback,
    };
}

test("a generic class method's Result returned from a generic fn") {
    assert(ok_or(get_it(new Box(5)), 0) == 5)?;
    let s = get_it(new Box("hi"));
    match s {
        Result::Ok(v) => assert(v == "hi")?,
        Result::Err(_) => assert(false)?,
    }
}

test("a Result local returned as is") {
    assert(ok_or(forward(4), 0) == 8)?;
    assert(ok_or(forward(-1), 7) == 7)?;
}

test("payload and whole Result returns in one function") {
    assert(ok_or(forward_or_payload(0), 0) == 100)?;
    assert(ok_or(forward_or_payload(3), 0) == 6)?;
    assert(ok_or(forward_or_payload(-3), 9) == 9)?;
}
