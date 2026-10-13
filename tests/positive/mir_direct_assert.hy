// `assert` in bodies lowered straight from HIR to MIR: `Ok(())` when the
// condition holds, else `Err` of the message, the default one when the
// call passes none.
use tests::assert;

fn check(int n) -> string {
    return match assert(n > 2, "too small") {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
}

fn check_default(int n) -> string {
    return match assert(n % 2 == 0) {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
}

test("assert passes and fails with its message") {
    assert!(check(5) == "ok");
    assert!(check(1) == "too small");
}

test("assert without a message fails with the default one") {
    assert!(check_default(4) == "ok");
    assert!(check_default(3) == "assertion failed");
}

test("assert in a numeric loop") {
    let acc = 0;
    let fails = 0;
    let i = 0;
    while i < 20 {
        acc = acc + i * 3;
        i = i + 1;
        match assert(acc % 2 < 1, "odd") {
            Result::Ok(_) => {},
            Result::Err(_) => {
                fails = fails + 1;
            },
        }
    }
    assert!(fails > 9 && fails < 11, "ten odd sums");
    assert!(acc > 569 && acc < 571);
    assert!(check_default(acc + 1) != "ok");
}

class Cell {
    pub v: int,
}

test("a failing assert in a heap body") {
    let c = new Cell(3);
    let n = c.v * 2 + 1;
    let r = match assert(n > 9, "small") {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
    let d = match assert(n % 2 == 0) {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
    assert!(r == "small" && d == "assertion failed");
}
