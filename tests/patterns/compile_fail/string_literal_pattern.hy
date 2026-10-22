// @expected E0001 — patterns take integer literals, not strings.
fn f(string s) -> int {
    return match s {
        "a" => 1,
        default => 0,
    };
}

fn main() {
    let r = f("a");
}
