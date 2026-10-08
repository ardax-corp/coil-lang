// A match on eight or more integer literals binary-searches its sorted
// cases: `<` tests halve them down to small leaves of equality tests, and
// a value that matches no case runs the catch-all.
#[repr(int)]
enum Status {
    Cont = 100,
    Ok = 200,
    Created = 201,
    Moved = 301,
    Bad = 400,
    Denied = 403,
    Missing = 404,
    Teapot = 418,
    Broken = 500,
    Huge = 5000000000,
}

fn word(int n) -> int {
    return match n {
        42 => 1,
        7 => 2,
        0 => 3,
        9000000000 => 4,
        13 => 5,
        1 => 6,
        100 => 7,
        3000000000 => 8,
        64 => 9,
        other => other * 1000,
    };
}

fn class(Status s) -> int {
    return match s {
        Status::Teapot => 9,
        Status::Ok => 2,
        Status::Missing => 7,
        Status::Cont => 1,
        Status::Broken => 10,
        Status::Created => 3,
        Status::Denied => 6,
        Status::Moved => 4,
        Status::Huge => 11,
        Status::Bad => 5,
    };
}

fn tally(int limit) -> int {
    let acc = 0;
    let i = 0;
    while i < limit {
        match i % 12 {
            0 => {
                acc = acc + 1;
            },
            2 => {
                acc = acc + 2;
            },
            3 => {
                acc = acc + 3;
            },
            5 => {
                acc = acc + 5;
            },
            7 => {
                acc = acc + 7;
            },
            8 => {
                acc = acc + 8;
            },
            10 => {
                acc = acc + 10;
            },
            11 => {
                acc = acc + 11;
            },
            default => {
                acc = acc + 100;
            },
        }
        i = i + 1;
    }
    return acc;
}

test("every literal finds its arm, and big literals work") {
    assert(word(42) == 1, "42")?;
    assert(word(7) == 2, "7")?;
    assert(word(0) == 3, "0")?;
    assert(word(9000000000) == 4, "big")?;
    assert(word(13) == 5, "13")?;
    assert(word(1) == 6, "1")?;
    assert(word(100) == 7, "100")?;
    assert(word(3000000000) == 8, "3e9")?;
    assert(word(64) == 9, "64")?;
}

test("a value between or beyond the cases binds the catch-all") {
    assert(word(2) == 2000, "between")?;
    assert(word(-8) == -8000, "below")?;
    assert(word(101) == 101000, "above")?;
    assert(word(9000000001) == 9000000001000, "past the top")?;
}

test("an exhaustive scalar enum match needs no catch-all") {
    assert(class(Status::Cont) == 1, "cont")?;
    assert(class(Status::Ok) == 2, "ok")?;
    assert(class(Status::Created) == 3, "created")?;
    assert(class(Status::Moved) == 4, "moved")?;
    assert(class(Status::Bad) == 5, "bad")?;
    assert(class(Status::Denied) == 6, "denied")?;
    assert(class(Status::Missing) == 7, "missing")?;
    assert(class(Status::Teapot) == 9, "teapot")?;
    assert(class(Status::Broken) == 10, "broken")?;
    assert(class(Status::Huge) == 11, "huge")?;
}

test("a statement match in a loop") {
    assert(tally(24) == 894, "two rounds")?;
}
