// Typed inlining of mono clone callees: the generic body is spliced in at
// the call's types.
fn add<T: Num>(T a, T b) -> T {
    return a + b;
}

fn first<T>(T a, T b) -> T {
    return a;
}

fn clamp_to<T: Num + Lt + Gt>(T x, T lo, T hi) -> T {
    if x < lo {
        return lo;
    }
    if x > hi {
        return hi;
    }
    return x;
}

fn wrap<T>(T x, bool keep) -> Option<T> {
    if keep {
        return Option::Some(x);
    }
    return Option::None;
}

fn sum_ints(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        acc = add(acc, clamp_to(i, 2, 5));
        i = i + 1;
    }
    return acc;
}

test("an int instance in a loop") {
    // i = 0..7 clamped to 2..5: 2+2+2+3+4+5+5+5.
    assert(sum_ints(8) == 28, "sum")?;
}

fn mixed(float x, int y) -> float {
    let f = add(x, 2.25);
    let k = add(y, 4);
    let a = first(k, 1);
    let b = first("a", "b");
    if b != "a" {
        return 0.0;
    }
    return f + (a as float);
}

test("one generic at two types in one caller") {
    assert(mixed(1.5, 3) == 10.75, "sum")?;
}

fn count_kept(int n) -> int {
    let c = 0;
    let i = 0;
    while i < n {
        c = c + match wrap(i, i % 3 == 0) {
            Option::Some(v) => v,
            Option::None => 0,
        };
        i = i + 1;
    }
    return c;
}

test("a generic Option result at int") {
    // 0 + 3 + 6 + 9.
    assert(count_kept(10) == 18, "sum")?;
}
