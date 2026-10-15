// A test body may use `?` on `Option` and on `Result`s with different error
// types in the same case, inside loops and nested blocks; each unwraps its
// success value.
use tests::assert;
use io::fs::exists;

#[derive(Show)]
enum Fault {
    Bad(int),
}

fn half(int n) -> Result<int, Fault> {
    if n % 2 != 0 {
        return Result::Err(Fault::Bad(n));
    }
    return n / 2;
}

fn parse(string s) -> Result<int, string> {
    if s == "ten" {
        return 10;
    }
    return Result::Err("unknown " + s);
}

fn first(Vec<int> xs) -> Option<int> {
    if xs.len() == 0 {
        return Option::None;
    }
    return Option::Some(xs[0]);
}

test("Option, Result<_, string> and a user error in one case") {
    let a = first(Vec::from([8, 1]))?;
    let b = half(a)?;
    let c = parse("ten")?;
    assert!(a + b + c == 22);
}

test("? inside a loop and a nested block") {
    let total = 0;
    for n in [2, 4, 6] {
        if n > 0 {
            total += half(n)?;
        }
    }
    assert!(total == 6);
}

test("? on a host Result") {
    assert!(exists(".")?);
}

test("? in a match arm") {
    let r = match first(Vec::from([4]))? {
        4 => half(4)?,
        default => -1,
    };
    assert!(r == 2);
}
