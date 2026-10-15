// Expected: E0001 — `case` is not a synonym of `match` (COI-74).
fn main() {
    case x { Option::None => 0, Option::Some(v) => v };
}
