// Expected: E0202 — `NoSuch::Missing(1)` names an enum that is not declared.
enum Real {
    Bar(int),
}

fn main() {
    let x = NoSuch::Missing(1);
}
