// Expected: E0134 — the expression panics while compiling.
fn boom(int x) -> int {
    if x > 2 {
        panic "too big";
    }
    return x;
}

fn main() {
    let x = comptime boom(5);
}
