// Scalar-backed enums: unboxed backing word, nominal type, coerce to backing.
#[repr(int)]
#[derive(Show, Eq, Ord, Hash)]
enum Status {
    Ok = 200,
    NotFound = 404,
}
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

fn take_int(int n) -> int {
    return n;
}

fn take_status(Status s) -> Status {
    return s;
}

test("int scalar match and coerce") {
    let s = Status::Ok;
    assert(describe(s) == 1)?;
    let n: int = Status::Ok;
    assert(n == 200)?;
    assert(Status::NotFound + 0 == 404)?;
    assert(describe(Status::NotFound) == 0)?;
    assert(take_int(Status::Ok) == 200)?;
    let kept = take_status(Status::NotFound);
    assert(describe(kept) == 0)?;
}

test("inferred repr without attribute") {
    assert(Status::Ok + Status::NotFound == 604)?;
}

test("string float bool scalar") {
    let mode: string = Mode::Fast;
    assert(mode == "fast")?;
    assert(Ratio::Half + 0.0 == 0.5)?;
    assert(Switch::On)?;
    let m = match Mode::Slow {
        Mode::Fast => 1,
        Mode::Slow => 2,
    };
    assert(m == 2)?;
}

test("Status.Ok next to Result.Ok") {
    let s = Status::Ok;
    let r = Result::Ok(1);
    assert(s + 0 == 200)?;
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

test("Status.Ok plus one") {
    assert(Status::Ok + 1 == 201)?;
}

fn hash_key<T: Hash>(T k) -> int {
    return k.hash();
}

test("derived Eq Hash Show Ord") {
    assert(Status::Ok == Status::Ok)?;
    assert(Status::Ok != Status::NotFound)?;
    assert(Status::Ok < Status::NotFound)?;
    assert(Status::Ok.show() == "Status.Ok")?;
    assert(hash_key(Status::Ok) == Status::Ok.hash())?;
    assert(Status::Ok.hash() != Status::NotFound.hash())?;
}

#[repr(int)]
#[derive(Ord)]
enum Rank {
    High = 10,
    Low = 1,
}

test("Ord uses backing word not declaration order") {
    assert(Rank::Low < Rank::High)?;
}
