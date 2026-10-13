// `#[repr(string)]` and `#[repr(bool)]` enums are their backing word in
// direct MIR: a variant is its string or bool, and a match tests it with
// `==` (a string by content).
#[repr(string)]
#[derive(Eq, Ord)]
enum Mode {
    Read = "r",
    Write = "w",
    Append = "a",
}

#[repr(bool)]
enum Gate {
    Shut = false,
    Open = true,
}

fn weight(Mode m) -> int {
    return match m {
        Mode::Read => 1,
        Mode::Write => 10,
        default => 100,
    };
}

fn pick(int n) -> Mode {
    if n == 0 {
        return Mode::Read;
    }
    if n == 1 {
        return Mode::Write;
    }
    return Mode::Append;
}

fn flipped(bool on) -> bool {
    let g = Gate::Shut;
    if on {
        g = Gate::Open;
    }
    return match g {
        Gate::Shut => true,
        Gate::Open => false,
    };
}

test("string and bool scalar enums") {
    assert(weight(pick(0)) + weight(pick(1)) + weight(pick(2)) == 111)?;
    assert(pick(1) == Mode::Write)?;
    assert(pick(2) != Mode::Read)?;
    assert(pick(0) < pick(1))?;
    assert(flipped(false))?;
    assert(!flipped(true))?;
}
