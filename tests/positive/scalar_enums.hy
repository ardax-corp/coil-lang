// Scalar-backed enums: unboxed backing word, nominal type, `.value`.
// Constructors are per-enum (`Status::Ok` next to prelude `Result::Ok`).
enum Status {
    Ok = 200,
    NotFound = 404,
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

#[repr(bool)]
enum Switch {
    Off = false,
    On = true,
}

enum Color {
    Red,
    Green,
}

fn describe(Status s) -> int {
    return match s {
        Status::Ok => 1,
        default => 0,
    };
}

test("int scalar match and value") {
    let s = Status::Ok;
    assert(describe(s) == 1)?;
    assert(s.value == 200)?;
    assert(Status::NotFound.value == 404)?;
    assert(describe(Status::NotFound) == 0)?;
}

test("inferred repr without attribute") {
    assert(Status::Ok.value + Status::NotFound.value == 604)?;
}

test("string float bool scalar") {
    assert(Mode::Fast.value == "fast")?;
    assert(Ratio::Half.value == 0.5)?;
    assert(Switch::On.value)?;
    let m = match Mode::Slow {
        Mode::Fast => 1,
        Mode::Slow => 2,
    };
    assert(m == 2)?;
}

test("Status.Ok next to Result.Ok") {
    let s = Status::Ok;
    let r = Result::Ok(1);
    assert(s.value == 200)?;
    assert(match r {
        Result::Ok(v) => v,
        Result::Err(_) => 0,
    } == 1)?;
}

test("payload enum still constructs") {
    let c = Color::Red;
    assert(match c {
        Color::Red => 1,
        Color::Green => 0,
    } == 1)?;
}
