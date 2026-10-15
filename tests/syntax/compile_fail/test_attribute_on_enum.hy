// Expected: E0119 — `#[test]` is rejected on an enum as on a fn; write `test("…") { … }`.
#[test]
enum E {
    A,
}

fn main() {}
