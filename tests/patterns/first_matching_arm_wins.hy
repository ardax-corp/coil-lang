// Arms are tried top to bottom and the first that matches runs, even when
// a later arm would match too: a literal before a wildcard slot, a nested
// literal before its binding, a specific variant before `default`.
use tests::assert;

fn code(Option<int> o) -> string {
    return match o {
        Option::Some(200) => "ok",
        Option::Some(404) => "missing",
        Option::Some(_) => "other",
        Option::None => "none",
    };
}

fn pick(Result<int, int> r) -> int {
    return match r {
        Result::Ok(0) => 1000,
        Result::Ok(v) => v,
        Result::Err(1) => -1000,
        Result::Err(e) => -e,
    };
}

fn small(int n) -> string {
    return match n {
        1 => "one",
        2 => "two",
        n => "many",
    };
}

test("literal payloads before a wildcard payload") {
    assert!(code(Option::Some(200)) == "ok");
    assert!(code(Option::Some(404)) == "missing");
    assert!(code(Option::Some(201)) == "other");
    assert!(code(Option::None) == "none");
}

test("literal before binding on both variants") {
    assert!(pick(Result::Ok(0)) == 1000);
    assert!(pick(Result::Ok(7)) == 7);
    assert!(pick(Result::Err(1)) == -1000);
    assert!(pick(Result::Err(5)) == -5);
}

test("literals before a binding on an int") {
    assert!(small(1) == "one");
    assert!(small(2) == "two");
    assert!(small(3) == "many");
    assert!(small(0) == "many");
}
