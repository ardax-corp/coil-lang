// Expected: E0134 — the initializer panics while compiling.
fn boom(int x) -> int {
    if x > 2 {
        panic("too big");
    }
    return x;
}

const B = boom(5);

fn main() {
}
