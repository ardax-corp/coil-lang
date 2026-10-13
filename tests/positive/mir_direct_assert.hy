// `assert!` in bodies lowered straight from HIR to MIR: the body goes on
// when the condition holds, else returns `Err` of the message, the default
// one (naming the condition) when the call passes none.
use tests::assert;

fn need(int n) -> Result<(), string> {
    assert!(n > 2, "too small");
    return Result::Ok(());
}

fn need_even(int n) -> Result<(), string> {
    assert!(n % 2 == 0);
    return Result::Ok(());
}

fn check(int n) -> string {
    return match need(n) {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
}

fn check_default(int n) -> string {
    return match need_even(n) {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
}

fn even_sum(int acc) -> Result<(), string> {
    assert!(acc % 2 < 1, "odd");
    return Result::Ok(());
}

test("assert passes and fails with its message") {
    assert!(check(5) == "ok");
    assert!(check(1) == "too small");
}

test("assert without a message fails with the default one") {
    assert!(check_default(4) == "ok");
    assert!(check_default(3) == "Assertion: 'n % 2 == 0' failed");
}

test("assert in a numeric loop") {
    let acc = 0;
    let fails = 0;
    let i = 0;
    while i < 20 {
        acc = acc + i * 3;
        i = i + 1;
        match even_sum(acc) {
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

fn big(Cell c) -> Result<int, string> {
    let n = c.v * 2 + 1;
    assert!(n > 9, "small");
    return Result::Ok(n);
}

test("a failing assert in a heap body") {
    let r = match big(new Cell(3)) {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
    let d = check_default(new Cell(3).v * 2 + 1);
    assert!(r == "small" && d == "Assertion: 'n % 2 == 0' failed");
}
