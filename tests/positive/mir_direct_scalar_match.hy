// `match` on an `int` or a scalar enum in bodies lowered straight from HIR
// to MIR: a test per arm, or a binary search over many literals, with the
// catch-all bound to the scrutinee.

#[repr(int)]
enum Level {
    Low = 1,
    Mid = 5,
    High = 9,
}

#[repr(float)]
enum Scale {
    Half = 0.5,
    Double = 2.0,
}

fn few(int n) -> int {
    return match n {
        0 => 10,
        1 => 20,
        other => other * 2,
    };
}

fn many(int n) -> int {
    return match n {
        1 => 1,
        0 => 2,
        3 => 3,
        8 => 4,
        13 => 5,
        21 => 6,
        34 => 7,
        55 => 8,
        4000000000 => 9,
        other => other + 1000,
    };
}

fn weight(Level l) -> int {
    return match l {
        Level::Low => 100,
        Level::Mid => 200,
        Level::High => 300,
    };
}

fn pick(int k) -> Level {
    if k < 3 {
        return Level::Low;
    }
    if k < 7 {
        return Level::Mid;
    }
    return Level::High;
}

fn scaled(Scale s, float x) -> float {
    return match s {
        Scale::Half => x * 0.5,
        Scale::Double => x * 2.0,
    };
}

fn tally(int limit) -> int {
    let acc = 0;
    let i = 0;
    while i < limit {
        match i % 4 {
            0 => {
                acc = acc + 1;
            },
            1 => {
                acc = acc + 10;
            },
            default => {
                acc = acc + 100;
            },
        }
        i = i + 1;
    }
    return acc;
}

test("int match with a binding catch-all") {
    assert(few(0) == 10)?;
    assert(few(1) == 20)?;
    assert(few(7) == 14)?;
}

test("binary search finds every literal and the catch-all") {
    assert(many(1) == 1)?;
    assert(many(0) == 2)?;
    assert(many(3) == 3)?;
    assert(many(8) == 4)?;
    assert(many(13) == 5)?;
    assert(many(21) == 6)?;
    assert(many(34) == 7)?;
    assert(many(55) == 8)?;
    assert(many(4000000000) == 9)?;
    assert(many(4) == 1004)?;
    assert(many(-6) == 994)?;
    assert(many(56) == 1056)?;
}

test("scalar enums are their backing words") {
    assert(weight(pick(0)) + weight(pick(5)) + weight(pick(9)) == 600)?;
    assert(scaled(Scale::Half, 8.0) == 4.0)?;
    assert(scaled(Scale::Double, 8.0) == 16.0)?;
}

test("statement match in a loop") {
    assert(tally(8) == 422)?;
}
