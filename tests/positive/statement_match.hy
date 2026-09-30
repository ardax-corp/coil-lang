// A `match` in statement position ends at its `}` like `if`: the `;` after
// it is optional.
enum Dir {
    Left,
    Right,
}

fn step(Dir d) -> int {
    match d {
        Dir::Left => {
            return -1;
        },
        Dir::Right => {
            return 1;
        },
    }
}

fn with_semicolon(Dir d) -> int {
    match d {
        Dir::Left => {
            return 10;
        },
        Dir::Right => {
            return 20;
        },
    }
}

test("statement match without semicolon") {
    assert(step(Dir::Left) == -1)?;
    assert(step(Dir::Right) == 1)?;
}

test("semicolon after statement match is still accepted") {
    assert(with_semicolon(Dir::Right) == 20)?;
}
