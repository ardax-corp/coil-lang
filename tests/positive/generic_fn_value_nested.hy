// A generic fn with a type parameter nested in its signature, used as a
// value: each call through the value gets the fn's typed copy, so the
// payload comes back as the caller's type, not a boxed `T` (#916).
class Box {
    pub v: int,
}

fn wrap<T>(T x) -> Option<T> {
    return Option::Some(x);
}

fn head<T>(Vec<T> v) -> T {
    return v[0];
}

fn pair<T>(T x) -> (T, T) {
    return (x, x);
}

fn single<T>(T x) -> Vec<T> {
    let v: Vec<T> = Vec::new();
    v.push(x);
    return v;
}

fn apply_to<T, U>(T x, T -> U f) -> U {
    return f(x);
}

fn take_f(float -> Option<float> f) -> Option<float> {
    return f(1.5);
}

fn some_or<T>(Option<T> o, T d) -> T {
    return match o {
        Option::Some(v) => v,
        Option::None => d,
    };
}

test("a let-bound generic fn value returns each payload kind") {
    let g = wrap;
    assert(some_or(g(2.5), 0.0) == 2.5)?;
    assert(some_or(g(7), 0) == 7)?;
    assert(some_or(g("s"), "") == "s")?;
    assert(some_or(g(true), false))?;
    assert(some_or(g(new Box(4)), new Box(0)).v == 4)?;
}

test("a generic fn passed to a typed fn parameter") {
    assert(some_or(take_f(wrap), 0.0) == 1.5)?;
    assert(some_or(apply_to(2.5, wrap), 0.0) == 2.5)?;
}

test("Vec, tuple and Vec-returning generic fns as values") {
    let h = head;
    let xs: Vec<float> = Vec::new();
    xs.push(3.5);
    assert(h(xs) == 3.5)?;
    let p = pair;
    let (a, b) = p(2.5);
    assert(a + b == 5.0)?;
    let s = single;
    assert(s(1.25)[0] == 1.25)?;
}
