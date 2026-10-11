// A free generic fn may return `Option<T>` (coil-lang#884): the shared body
// keeps `T` raw, so every payload kind round-trips.
class Pt {
    pub x: int,
    pub y: int,
}

enum Sh {
    A(int),
    B,
}

fn wrap<T>(T x) -> Option<T> {
    return Option::Some(x);
}

fn none<T>(T x) -> Option<T> {
    return Option::None;
}

fn pair<T>(T x) -> Option<(T, T)> {
    return Option::Some((x, x));
}

fn first<T>(Vec<T> xs) -> Option<T> {
    if xs.len() == 0 {
        return Option::None;
    }
    return Option::Some(xs[0]);
}

fn get_or<T>(Option<T> o, T d) -> T {
    match o {
        Option::Some(v) => {
            return v;
        },
        Option::None => {
            return d;
        },
    }
}

test("wrap every payload kind") {
    assert(get_or(wrap(3), 0) == 3)?;
    assert(get_or(wrap(2.5), 0.0) == 2.5)?;
    assert(get_or(wrap("ab"), "") == "ab")?;
    assert(get_or(wrap(true), false))?;
    assert(get_or(wrap(7 as byte), 0 as byte) == (7 as byte))?;
    assert(get_or(wrap(new Pt(1, 2)), new Pt(0, 0)).y == 2)?;
    assert(get_or(none(5), 9) == 9)?;
}

test("match on the result") {
    match wrap(Sh::A(4)) {
        Option::Some(Sh::A(n)) => {
            assert(n == 4)?;
        },
        default => {
            assert(false)?;
        },
    }
    match wrap(1.5) {
        Option::Some(f) => {
            assert(f * 2.0 == 3.0)?;
        },
        Option::None => {
            assert(false)?;
        },
    }
    match pair(0.5) {
        Option::Some(p) => {
            let (a, b) = p;
            assert(a + b == 1.0)?;
        },
        Option::None => {
            assert(false)?;
        },
    }
}

test("first of a Vec") {
    assert(get_or(first(Vec::from(["ab", "cd"])), "") == "ab")?;
    assert(get_or(first(Vec::from([1.5, 2.5])), 0.0) == 1.5)?;
    let empty: Vec<int> = Vec::new();
    assert(get_or(first(empty), -1) == -1)?;
}
