// `assert!` in a helper function returns `Err` from that helper (it does
// not abort), so the caller can match the message or forward it with `?`.
use tests::assert;

fn positive(int n) -> Result<int, string> {
    assert!(n > 0, "not positive");
    return Result::Ok(n);
}

fn all_positive(Vec<int> xs) -> Result<int, string> {
    let total = 0;
    for x in xs {
        total = total + positive(x)?;
    }
    return Result::Ok(total);
}

fn err_of(Result<int, string> r) -> string {
    return match r {
        Result::Ok(_) => "",
        Result::Err(e) => e,
    };
}

test("a failed assert is an Err with its message") {
    assert!(positive(3) == Result::Ok(3));
    assert!(err_of(positive(0)) == "not positive");
    assert!(err_of(positive(-7)) == "not positive");
}

test("? forwards the assert's Err") {
    assert!(all_positive([1, 2, 3]) == Result::Ok(6));
    assert!(err_of(all_positive([1, -2, 3])) == "not positive");
}

test("the caller keeps running after a helper's assert fails") {
    let r = positive(-1);
    let after = r ?? 99;
    assert!(after == 99);
}
