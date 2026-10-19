// Expected: E0119 — a single-parameter bound on the function's own type
// parameter is written `T: Num`; `where Num<T>` is for multi-parameter traits
// and for a method bounding its impl's parameter.
fn double<T>(T x) -> T where Num<T> {
    return x + x;
}

fn main() {
    let y = double(2);
}
