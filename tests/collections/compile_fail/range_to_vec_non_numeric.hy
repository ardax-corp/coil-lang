// Expected: E0119 — .to_vec() needs int/byte/float, not generic Ord.
fn dump<T: Ord>(T a, T b) {
    let _ = (a..b).to_vec();
}

fn main() {}
