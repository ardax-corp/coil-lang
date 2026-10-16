// Expected: E0122 — a name overloaded by arity is ambiguous as a value.
fn add(int x) -> int {
    return x;
}

fn add(int x, int y) -> int {
    return x + y;
}

fn main() {
    let f = add;
}
