// Expected: E0119 — inclusive .to_vec() needs int/byte/float.
fn dump<T: Ord>(T a, T b) {
    let _ = (a..=b).to_vec();
}

fn main() {}
