// Expected: E0001 — `case` with a single arm is not a `match` (COI-74).
fn main() {
    case x { Option::None => 0 };
}
