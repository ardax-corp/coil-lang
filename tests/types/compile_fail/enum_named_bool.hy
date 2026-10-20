// Expected: E0200 — `Bool` names the built-in `bool`; an enum cannot take the
// name (#1673).
enum Bool {
    Yes,
    No,
}

fn yes(Bool b) -> bool {
    return match b {
        Bool::Yes => true,
        Bool::No => false,
    };
}

fn main() {
    let _ = yes(Bool::Yes);
}
