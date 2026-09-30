// A match arm that reaches the join by fall-through keeps its own value when
// the other arms end in the same constant / binop / suffix (return convoy).

fn check(int n) -> Result<(), string> {
    if n > 100 {
        return Result::Err("big");
    }
    return Result::Ok(());
}

fn find(int n) -> Option<string> {
    if n > 0 {
        return Option::Some("found");
    }
    return Option::None;
}

enum Mode {
    Zero,
    Other(int),
}

class Counter {
    pub value: int,
}

impl Counter {
    pub fn get() -> int {
        return self.value;
    }

    // Zero's arm ends in a CALL and falls into the join; Other's LOAD of `n`
    // must not be sunk there.
    pub fn describe(Mode m) -> int {
        return match m {
            Mode::Zero => {
                self.get()
            },
            Mode::Other(n) => {
                self.get();
                n
            },
        };
    }
}

fn unit_result_arm(int n) -> int {
    return match check(n) {
        Result::Ok(_) => 1,
        Result::Err(e) => e.len(),
    };
}

fn niche_option_arm(int n) -> int {
    return match find(n) {
        Option::Some(s) => s.len(),
        Option::None => 1,
    };
}

fn int_arm(int k, [int] xs) -> int {
    return match k {
        0 => 1,
        1 => 1,
        default => xs.len(),
    };
}

fn binop_arm(int k, int a, int b, string s) -> int {
    return match k {
        0 => a + b,
        1 => a + b,
        default => s.len(),
    };
}

fn suffix_arm(int k, int a, string s) -> int {
    return match k {
        0 => a * 2 + 1,
        1 => a * 2 + 1,
        default => s.len(),
    };
}

test("niche unit Result: Err arm keeps its value") {
    assert(unit_result_arm(1004) == 3)?;
    assert(unit_result_arm(5) == 1)?;
}

test("niche Option: Some arm keeps its value") {
    assert(niche_option_arm(1) == 5)?;
    assert(niche_option_arm(0) == 1)?;
}

test("int match: default arm keeps its value") {
    assert(int_arm(0, [4, 5, 6]) == 1)?;
    assert(int_arm(2, [4, 5, 6]) == 3)?;
}

test("binop arms: default arm keeps its value") {
    assert(binop_arm(0, 2, 3, "four") == 5)?;
    assert(binop_arm(2, 2, 3, "four") == 4)?;
}

test("suffix arms: default arm keeps its value") {
    assert(suffix_arm(1, 4, "abcdefg") == 9)?;
    assert(suffix_arm(2, 4, "abcdefg") == 7)?;
}

test("call arm falls into join past a sunk LOAD") {
    let c = new Counter(5);
    assert(c.describe(Mode::Zero) == 5)?;
    assert(c.describe(Mode::Other(9)) == 9)?;
}
