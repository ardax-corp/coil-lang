// `invoke`'s function id is any `int` expression, not only a local, a field
// or a `static` (#1788). With no library loaded, each call fails with
// `InvalidHandle` rather than the compiler refusing to lower it.
use tests::assert;
use ffi::{invoke, Error, ErrorKind};

fn kind_of(Result<int, Error> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(e) => match e.kind {
            ErrorKind::InvalidHandle => "InvalidHandle",
            default => "other",
        },
    };
}

fn next_id(int base) -> int {
    return base + 1;
}

test("invoke with a literal function id") {
    assert!(kind_of(invoke(7, 0, (1, 2))) == "InvalidHandle");
}

test("invoke with an arithmetic function id") {
    let base = 2;
    assert!(kind_of(invoke(7, base * 3 + 1, (1, 2))) == "InvalidHandle");
}

test("invoke with a call as the function id") {
    assert!(kind_of(invoke(7, next_id(4), (1, 2))) == "InvalidHandle");
}
