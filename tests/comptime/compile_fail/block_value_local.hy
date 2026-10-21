// Expected: E0100 — a `comptime { … }` block runs while compiling, so it
// cannot use the function's parameters or local variables.
fn twice(int k) -> int {
    return comptime {
        return k * 2;
    };
}

fn main() {
    let x = twice(2);
}
