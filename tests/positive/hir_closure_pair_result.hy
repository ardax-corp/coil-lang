// A call through a function value whose result is a two-word enum
// (`Result<int, string>`, `Option<int>`) gets the boxed enum back.

fn parse(int n, int fail) -> Result<int, string> {
    if fail == 1 {
        raise "bad";
    }
    return n;
}

fn half(int n) -> Option<int> {
    if n % 2 == 1 {
        return Option::None;
    }
    return Option::Some(n / 2);
}

fn ok_or(Result<int, string> r, int d) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => d,
    };
}

fn apply(int -> Option<int> f, int x) -> int {
    return f(x) ?? -1;
}

test("named function value") {
    let f = parse;
    assert(ok_or(f(7, 0), -1) == 7)?;
    assert(ok_or(f(7, 1), -1) == -1)?;
}

test("lambda over a pair result") {
    let g = fn (int x) => parse(x * 2, 0);
    assert(ok_or(g(4), 0) == 8)?;
    let h = fn (int x) => parse(x, 1);
    assert(ok_or(h(4), 3) == 3)?;
}

test("option through a parameter") {
    assert(apply(half, 10) == 5)?;
    assert(apply(half, 3) == -1)?;
    assert(apply(fn (int x) => half(x + 1), 5) == 3)?;
}
