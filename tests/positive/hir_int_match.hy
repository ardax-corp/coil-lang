// `match` on an `int` with literal arms: compare-and-branch per arm, the
// last one untested, a binding storing the scrutinee.
fn grade(int score) -> int {
    return match score {
        100 => 4,
        90 => 3,
        0 => -10,
        default => 0,
    };
}

fn shifted(int n) -> int {
    match n {
        0 => {
            return 1;
        },
        other => {
            return other + 5;
        },
    }
}

fn huge(int n) -> int {
    return match n {
        6000000000 => 2,
        5000000000 => 1,
        default => 0,
    };
}

fn operand(int a, int k) -> int {
    return a * 10 + match k {
        1 => 7,
        default => k,
    };
}

fn count_sevens([int] xs) -> int {
    let hits = 0;
    for x in xs {
        match x {
            7 => {
                hits = hits + 1;
            },
            default => {},
        }
    }
    return hits;
}

fn only_default(int n) -> int {
    return match n {
        default => n * 2,
    };
}

test("int literal arms and default") {
    assert(grade(100) == 4)?;
    assert(grade(90) == 3)?;
    assert(grade(0) == -10)?;
    assert(grade(55) == 0)?;
}

test("binding catch-all and statement match") {
    assert(shifted(0) == 1)?;
    assert(shifted(3) == 8)?;
    assert(count_sevens([7, 1, 7, 7, 2]) == 3)?;
}

test("literals outside i32 and a match as an operand") {
    assert(huge(5000000000) == 1)?;
    assert(huge(6000000000) == 2)?;
    assert(huge(5) == 0)?;
    assert(huge(-5000000000) == 0)?;
    assert(operand(2, 1) == 27)?;
    assert(operand(2, 4) == 24)?;
    assert(only_default(21) == 42)?;
}
