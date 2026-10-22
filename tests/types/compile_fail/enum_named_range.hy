// @expected E0200 — `Range` is the built-in `Range<T>`; an enum cannot
// redeclare it (#1654).
enum Range {
    Open,
    Closed,
}

fn is_open(Range r) -> bool {
    return match r {
        Range::Open => true,
        Range::Closed => false,
    };
}

fn main() {
    let _ = is_open(Range::Open);
}
