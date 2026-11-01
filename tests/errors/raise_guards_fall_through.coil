// A `()` helper whose guards `raise` returns `Result<(), E>`; falling past
// its last `if c { raise e; }` returns `Ok(())`. That `if` has no `else`,
// so it is `()`, not the `never` of its branch, and the helper keeps its
// implicit `Ok(())`. Inlined as a statement into an unrolled loop, its
// arms are `Err` and `Ok` words; they once met `()` instead and the body
// refused with "no MIR from HIR: arm value". The callers drop the
// `Result` on purpose (an E0136 warning outside a `test` body).
use tests::assert;

fn check(int n) {
    let r = n * 2;
    if r < 0 {
        raise "negative";
    }
    if r > 100 {
        raise "too big";
    }
}

fn cube(int n) {
    let r = (n * 1000 + 7) as float;
    assert!(r * r > 0.0);
    assert!(0.0 - r < 0.0);
}

fn cube_three() -> int {
    let n = 0;
    while n < 3 {
        cube(n);
        n = n + 1;
    }
    return n;
}

fn check_three() -> int {
    let n = 0;
    while n < 3 {
        check(n);
        n = n + 1;
    }
    return n;
}

fn err_of(Result<(), string> r) -> string {
    return match r {
        Result::Ok(_) => "",
        Result::Err(e) => e,
    };
}

test("a raising helper called as a statement in a loop") {
    assert!(cube_three() == 3);
    assert!(check_three() == 3);
}

test("falling past the last guard is Ok") {
    assert!(check(10) == Result::Ok(()));
    assert!(err_of(check(-1)) == "negative");
    assert!(err_of(check(51)) == "too big");
}
