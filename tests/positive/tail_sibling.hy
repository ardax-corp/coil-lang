// Sibling / self tail-call elim: even↔odd and a two-word bounce.

fn even(int n) -> int {
    if n == 0 {
        return 1;
    }
    return odd(n - 1);
}

fn odd(int n) -> int {
    if n == 0 {
        return 0;
    }
    return even(n - 1);
}

fn bounce_a(Option<int> o) -> Option<int> {
    return bounce_b(o);
}

fn bounce_b(Option<int> o) -> Option<int> {
    return match o {
        Option::None => Option::Some(1),
        Option::Some(x) => bounce_a(Option::None),
    };
}

test("sibling tail even/odd") {
    assert(even(10) == 1)?;
    assert(odd(10) == 0)?;
    assert(even(1) == 0)?;
    assert(odd(1) == 1)?;
}

test("two-word sibling tail") {
    let r = bounce_a(Option::None);
    let ok = match r {
        Option::Some(v) => v == 1,
        Option::None => false,
    };
    assert(ok)?;
}
