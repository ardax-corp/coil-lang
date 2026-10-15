// A call whose result is an enum kept as one boxed object (no pair or
// niche layout: an `Ok` payload that is itself an enum) comes back as that
// object's word, and direct MIR reads it the same way: matched in place,
// bound to a local, or passed on.
use tests::assert;

fn parse(int n, int fail) {
    if fail == 1 {
        raise "bad";
    }
    return n;
}

fn maybe(int fail, int empty) -> Result<Option<int>, string> {
    let value = parse(7, fail)?;
    if empty == 1 {
        return Option::None;
    }
    return Option::Some(value);
}

fn nested(int fail) -> Result<Result<int, string>, string> {
    let value = parse(5, fail)?;
    return Result::Ok(value);
}

fn payload(Result<Option<int>, string> r) -> int {
    return match r {
        Result::Ok(Option::Some(v)) => v,
        Result::Ok(Option::None) => 0,
        Result::Err(_) => -1,
    };
}

test("a boxed enum result matched, bound and passed on") {
    assert!(payload(maybe(0, 0)) == 7);
    assert!(payload(maybe(0, 1)) == 0);
    assert!(payload(maybe(1, 0)) == -1);
    let r = maybe(0, 0);
    let again = r;
    assert!(payload(again) == 7);
    assert!(
        match nested(0) {
            Result::Ok(Result::Ok(v)) => v == 5,
            default => false,
        },
    );
    assert!(match nested(1) {
        Result::Err(e) => e == "bad",
        default => false,
    });
}
