// A named function returning a one-word enum can be passed on as a value
// and called through it, and its result keeps its variant and payload.
use tests::assert;
use thread::{join, spawn};

fn name_of(int x) -> Option<string> {
    if x % 2 == 0 {
        return Option::Some("even");
    }
    return Option::None;
}

fn checked(int x) -> Result<(), string> {
    if x < 0 {
        return Result::Err("negative");
    }
    return Result::Ok(());
}

fn apply(int -> Option<string> f, int x) -> string {
    return f(x) ?? "odd";
}

fn worker(int n) {
    checked(n)?;
}

fn passes(int x) -> string {
    let f = name_of;
    return apply(f, x);
}

test("function value with a niche Option result") {
    assert!(passes(8) == "even");
    assert!(passes(7) == "odd");
    assert!(apply(name_of, 10) == "even");
}

test("spawn a function returning a unit Result") {
    let t = spawn(worker, 3)?;
    join(t)?;
}
