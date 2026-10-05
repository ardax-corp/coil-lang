// Scalar-backed enums are their backing word: a variant pushes its
// constant, a match compares it, and the value moves like any word.
#[repr(int)]
enum Code {
    Ok = 200,
    Missing = 404,
    Big = 5000000000,
}

#[repr(string)]
enum Mode {
    Fast = "fast",
    Slow = "slow",
}

#[repr(float)]
enum Ratio {
    Half = 0.5,
    Full = 1.0,
}

fn rank(Code c) -> int {
    return match c {
        Code::Ok => 1,
        Code::Missing => 2,
        Code::Big => 3,
    };
}

fn pick(int k) -> Code {
    if k == 0 {
        return Code::Ok;
    }
    if k == 1 {
        return Code::Missing;
    }
    return Code::Big;
}

fn speed(Mode m) -> int {
    match m {
        Mode::Fast => {
            return 10;
        },
        other => {
            return speed_of(other);
        },
    }
}

fn speed_of(Mode m) -> int {
    return match m {
        Mode::Slow => 1,
        default => 0,
    };
}

fn weight(Ratio r) -> int {
    return match r {
        Ratio::Half => 50,
        Ratio::Full => 100,
    };
}

fn backing(Code c) -> int {
    let n: int = c;
    return n;
}

test("int-backed variants, returns and matches") {
    assert(rank(pick(0)) == 1)?;
    assert(rank(pick(1)) == 2)?;
    assert(rank(pick(2)) == 3)?;
    assert(backing(Code::Missing) == 404)?;
    assert(backing(Code::Big) == 5000000000)?;
}

test("string and float backings") {
    assert(speed(Mode::Fast) == 10)?;
    assert(speed(Mode::Slow) == 1)?;
    assert(weight(Ratio::Half) == 50)?;
    assert(weight(Ratio::Full) == 100)?;
}
