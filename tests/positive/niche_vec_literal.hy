// An array literal of niche elements (`Option<string>`, `Result<string, E>`,
// `Result<unit, E>`) bound to a `Vec`-typed local reads each element back
// as the variant it was built as, whether read by literal or computed
// index, passed on, or after the Vec escapes.
use tests::assert;

enum E1 {
    Bad,
}

fn is_some(Option<string> o) -> bool {
    return match o {
        Option::Some(_) => true,
        default => false,
    };
}

fn is_ok_str(Result<string, E1> r) -> bool {
    return match r {
        Result::Ok(_) => true,
        default => false,
    };
}

fn is_ok(Result<unit, E1> r) -> bool {
    return match r {
        Result::Ok(_) => true,
        default => false,
    };
}

fn count_some(Vec<Option<string>> v) -> int {
    let n = 0;
    for o in v {
        if is_some(o) {
            n = n + 1;
        }
    }
    return n;
}

test("Option<string> literal in a Vec local") {
    let v: Vec<Option<string>> = [Option::None, Option::Some("a")];
    let none = match v[0] {
        Option::None => true,
        default => false,
    };
    assert!(none);
    assert!(!is_some(v[0]));
    assert!(is_some(v[1]));
    let i = 1;
    assert!(is_some(v[i]));
    assert!(!is_some(v[i - 1]));
}

test("Result<string, E> literal in a Vec local") {
    let v: Vec<Result<string, E1>> = [Result::Err(E1::Bad), Result::Ok("a")];
    assert!(!is_ok_str(v[0]));
    assert!(is_ok_str(v[1]));
}

test("Result<unit, E> literal in a Vec local") {
    let v: Vec<Result<unit, E1>> = [Result::Ok(()), Result::Err(E1::Bad)];
    assert!(is_ok(v[0]));
    assert!(!is_ok(v[1]));
}

test("niche literal stored into, then passed whole") {
    let v: Vec<Option<string>> = [Option::None, Option::None, Option::Some("c")];
    v[1] = Option::Some("b");
    assert!(is_some(v[1]));
    assert!(count_some(v) == 2);
}
