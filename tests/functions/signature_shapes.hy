// Bodies whose direct MIR once failed its verifier: a function returning a
// function (the curried signature peeled past its parameters), an overload
// (typed by the last overload of its name), a `Result<(), E>` body ending in
// `f()?;`, and a `bool` string native.
use tests::assert;
use string::match_at;

fn adder(int k) -> fn(int x) -> int {
    return fn (int y) use (k) => y + k;
}

fn pick(bool hi) -> int -> int {
    if hi {
        return fn (int x) => x + 1;
    }
    return fn (int x) => x - 1;
}

fn mix(float a, float b) -> float {
    return a * b + 0.5;
}

fn mix(int a, int b) -> int {
    return a * b + 1;
}

fn counted(int n) -> Result<int, string> {
    if n < 0 {
        raise "negative";
    }
    return Result::Ok(n * 2);
}

fn tail_try(int n) -> Result<(), string> {
    let f = fn () use (n) => counted(n);
    f()?;
}

fn ok(Result<(), string> r) -> bool {
    return match r {
        Result::Ok(_) => true,
        Result::Err(_) => false,
    };
}

fn starts(string s, string p) -> bool {
    let hit = match_at(s, p, 0);
    return hit;
}

test("returned closures keep their function type") {
    let add3 = adder(3);
    assert!(add3(4) == 7);
    let up = pick(true);
    let down = pick(false);
    assert!(up(4) == 5);
    assert!(down(4) == 3);
}

test("each overload its own signature") {
    assert!(mix(2.0, 3.0) == 6.5);
    assert!(mix(2, 3) == 7);
}

test("a tail try in a unit result body") {
    assert!(ok(tail_try(2)));
    assert!(!ok(tail_try(0 - 1)));
}

test("a bool string native") {
    assert!(starts("coil", "co"));
    assert!(!starts("coil", "il"));
}
