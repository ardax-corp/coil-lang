// A statement `match` whose arms leave different numbers of values (`{}` vs
// an expression) must not disturb the locals after it. Each arm drops its
// own value; nothing is popped after the match.
enum Dir {
    Left,
    Right,
}

#[repr(int)]
enum Level {
    Low = 1,
    High = 2,
}

fn noop() {}

fn two() -> int {
    return 2;
}

fn empty_arms(Dir d) -> int {
    let xs = [5, 6];
    match d {
        Dir::Left => {
            noop();
        },
        Dir::Right => {},
    };
    -xs[0];
    return 7 + xs[1];
}

fn mixed_arms(Dir d) -> int {
    let xs = [5, 6];
    match d {
        Dir::Left => noop(),
        Dir::Right => {},
    };
    -xs[0];
    return 7 + xs[1];
}

fn option_arms(Option<int> o) -> int {
    let xs = [5, 6];
    match o {
        Option::Some(v) => {
            let _ = v;
            noop()
        },
        Option::None => {},
    };
    -xs[0];
    return 7 + xs[1];
}

fn scalar_arms(Level l) -> int {
    let xs = [5, 6];
    match l {
        Level::Low => {},
        Level::High => noop(),
    };
    -xs[0];
    return 7 + xs[1];
}

fn tail_value_arm(Dir d) -> int {
    let xs = [5, 6];
    match d {
        Dir::Left => {
            let _ = two();
            noop()
        },
        Dir::Right => {},
    };
    -xs[0];
    return 7 + xs[1];
}

test("empty block arms") {
    assert(empty_arms(Dir::Left) == 13)?;
    assert(empty_arms(Dir::Right) == 13)?;
}

test("expression arm next to an empty arm") {
    assert(mixed_arms(Dir::Left) == 13)?;
    assert(mixed_arms(Dir::Right) == 13)?;
}

test("option statement match") {
    assert(option_arms(Option::Some(1)) == 13)?;
    assert(option_arms(Option::None) == 13)?;
}

test("scalar enum statement match") {
    assert(scalar_arms(Level::Low) == 13)?;
    assert(scalar_arms(Level::High) == 13)?;
}

test("arm with a tail value") {
    assert(tail_value_arm(Dir::Left) == 13)?;
    assert(tail_value_arm(Dir::Right) == 13)?;
}
