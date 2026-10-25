// A lambda's `raise` / `?` make the lambda Result- or Option-returning,
// not the function or test around it (#1164).
use tests::assert;

fn half(int n) -> Option<int> {
    if n % 2 == 0 {
        return Option::Some(n / 2);
    }
    return Option::None;
}

fn apply(int -> Result<int, string> f, int n) -> Result<int, string> {
    return f(n);
}

fn raise_in_main_like() -> int {
    let check = fn (int n) {
        if n < 0 {
            raise "neg";
        }
        return n + 1;
    };
    return (check(-1).ok() ?? 0) + (check(4).ok() ?? 0);
}

test("a raising lambda returns a Result") {
    let check = fn (int n) {
        if n < 0 {
            raise "neg";
        }
        return n + 1;
    };
    assert!((check(-1).ok() ?? 0) == 0);
    assert!((check(2).ok() ?? 0) == 3);
    assert!(apply(check, 1) == Result::Ok(2));
    assert!(apply(check, -5) == Result::Err("neg"));
}

test("a raising lambda inside a plain function") {
    assert!(raise_in_main_like() == 5);
}

test("an Option `?` lambda returns an Option") {
    let quarter = fn (int n) {
        let h = half(n)?;
        return Option::Some(half(h)?);
    };
    assert!(quarter(8) == Option::Some(2));
    assert!(quarter(6) == Option::None);
    assert!(quarter(3) == Option::None);
}

test("explicit Result returns in a lambda") {
    let check = fn (int n) {
        if n < 0 {
            return Result::Err("neg");
        }
        return Result::Ok(n + 1);
    };
    assert!(apply(check, 1) == Result::Ok(2));
    assert!(apply(check, -1) == Result::Err("neg"));
}
