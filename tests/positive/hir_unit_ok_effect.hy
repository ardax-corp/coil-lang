// A `()`-valued expression returned as `Ok` from a `Result<(), E>` runs for
// its effect.

static let seen: int = 0;

fn note() {
    seen = seen + 1;
}

fn step(int x) -> Result<(), int> {
    return match x {
        0 => note(),
        default => raise x,
    };
}

test("unit expression as the Ok payload") {
    seen = 0;
    let ok = match step(0) {
        Result::Ok(_) => 1,
        Result::Err(_) => 0,
    };
    assert(ok == 1)?;
    assert(seen == 1)?;
    let err = match step(4) {
        Result::Ok(_) => 0,
        Result::Err(e) => e,
    };
    assert(err == 4)?;
    assert(seen == 1)?;
}
