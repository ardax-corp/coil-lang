// Expected: E0122 — an overloaded name is ambiguous as a value.
fn kind(int x) -> string {
    return "i";
}

fn kind(float x) -> string {
    return "f";
}

fn main() {
    let g = kind;
    let r = g(4);
}
