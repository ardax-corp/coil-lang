// A `match` that binds payload slots inside a host native's argument
// (`to_bytes(format("%i", match ..))`) runs before the native's id is
// pushed, so its slots do not land on the id.
use string::{format, to_bytes};

enum Wrap {
    Empty,
    Full(int),
}

fn width(int pad, Vec<byte> bytes) -> int {
    return pad + len(bytes);
}

fn shown(Wrap w) -> int {
    return width(
        10,
        to_bytes(
            format(
                "%i",
                match w {
                    Wrap::Empty => 0,
                    Wrap::Full(v) => v * 100,
                },
            ),
        ),
    );
}

fn try_shown(Result<int, string> r) -> Result<int, string> {
    return Result::Ok(width(1, to_bytes(format("%i", r?))));
}

test("a binding match under a host call") {
    assert(shown(Wrap::Full(42)) == 14)?;
    assert(shown(Wrap::Empty) == 11)?;
}

test("a `?` under a host call") {
    match try_shown(Result::Ok(123)) {
        Result::Ok(n) => assert(n == 4)?,
        Result::Err(_) => assert(false)?,
    }
}
