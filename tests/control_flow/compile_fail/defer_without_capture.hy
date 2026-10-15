// Expected: E0100 — a `defer` body cannot read an outer local unless it
// lists it in `use (…)`.
fn f(int n) {
    defer {
        let m = n;
    }
}

fn main() {
    f(1);
}
