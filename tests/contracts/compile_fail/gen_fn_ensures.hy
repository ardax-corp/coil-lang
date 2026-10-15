// Expected: E0119 — a `gen fn` cannot have an `ensures` yet.
gen fn count(int n) -> int
    ensures result > 0
{
    yield n;
    return n;
}

fn main() {
    let c = count(1);
}
