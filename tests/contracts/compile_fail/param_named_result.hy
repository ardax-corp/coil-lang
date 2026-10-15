// Expected: E0119 — a function with `ensures` cannot have a parameter named `result`.
fn f(int result) -> int
    ensures result > 0
{
    return result;
}

fn main() {
    f(1);
}
