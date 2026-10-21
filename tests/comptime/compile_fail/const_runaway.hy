// Expected: E0134 — the initializer runs past the step budget.
// Flags: --comptime-steps 1000000
fn spin(int n) -> int {
    let i = 0;
    while i >= 0 {
        i = i + n;
    }
    return i;
}

const S = spin(0);

fn main() {}
