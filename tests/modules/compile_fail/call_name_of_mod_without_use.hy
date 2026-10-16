// Expected: E0101 — `mod math;` binds no names, so a bare `add` is not a
// function here (#1211).
mod math;

fn main() {
    let _ = add(1, 2);
}
