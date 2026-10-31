// `match` is an expression: it can be a call argument, an operand of `+`
// and string concatenation, a lambda body, a match arm's value, and the
// condition of an `if`.
use tests::assert;

fn sign(int n) -> int {
    return match n {
        0 => 0,
        default => 1,
    };
}

fn twice(int n) -> int {
    return n * 2;
}

test("as a call argument") {
    let o = Option::Some(4);
    assert!(twice(match o {
        Option::Some(v) => v,
        Option::None => 0,
    }) == 8);
}

test("as operands") {
    let a = Option::Some(3);
    let n = 10 + match a {
        Option::Some(v) => v,
        Option::None => 0,
    };
    assert!(n == 13);
    let s = "x" + match a {
        Option::Some(_) => "some",
        Option::None => "none",
    };
    assert!(s == "xsome");
}

test("as a lambda body") {
    let f = fn (int n) => match n {
        1 => "one",
        default => "many",
    };
    assert!(f(1) == "one");
    assert!(f(5) == "many");
}

test("as an arm value") {
    let r: Result<Option<int>, string> = Result::Ok(Option::None);
    let v = match r {
        Result::Ok(inner) => match inner {
            Option::Some(x) => x,
            Option::None => -1,
        },
        Result::Err(_) => -2,
    };
    assert!(v == -1);
}

test("as an if condition") {
    let hit = 0;
    if match sign(5) {
        1 => true,
        default => false,
    } {
        hit = 1;
    }
    assert!(hit == 1);
}
