// @expected E0111 — a `while` that is not `while true` may end, so the
// code after it still needs a `return`.
fn find(int n) -> int {
    let i = 0;
    while i < n {
        if i == 3 {
            return i;
        }
        i = i + 1;
    }
}

fn main() {
    find(5);
}
