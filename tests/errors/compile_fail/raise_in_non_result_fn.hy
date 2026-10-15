// Expected: E0102 — a function annotated `-> int` cannot `raise`.
fn f(int n) -> int {
    if n > 0 {
        raise "positive";
    }
    return n;
}

fn main() {
    let r = f(1);
}
