// @expected E0115 — `??` takes an Option only: on a Result it would drop the
// error without a word. Spell the fallback `.unwrap_or(0)` or `.ok() ?? 0` (#1878).
use conv::parse_int;

fn main() {
    let n = parse_int("x") ?? 0;
    let _ = n;
}
